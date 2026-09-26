//! Read-only portal data: agents/skills, memory search, tasks, health, stats.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use super::error::PortalError;
use super::PortalState;

fn truncate_chars(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() > max {
        format!("{}…", t.chars().take(max).collect::<String>())
    } else {
        t.to_string()
    }
}

// ---------------------------------------------------------------------------
// Agents & skills
// ---------------------------------------------------------------------------

/// GET /api/agents — the main agent plus loaded subagent definitions.
pub async fn agents(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let model = state.agent.current_model().await;
    let web_active = state.agent.is_processing(&state.config.user_name).await;
    let mut telegram_active = false;
    for id in &state.agent.config().telegram.allowed_user_ids {
        if state.agent.is_processing(&id.to_string()).await {
            telegram_active = true;
            break;
        }
    }

    let mut out = Vec::new();
    let status = if web_active || telegram_active {
        "running"
    } else {
        "idle"
    };
    out.push(json!({
        "id": "main",
        "name": "rustfox",
        "model": model,
        "status": status,
        "lastActive": chrono::Utc::now().to_rfc3339(),
        "platform": "telegram+web",
    }));

    for skill in state.agent.agent_entries().await {
        out.push(json!({
            "id": format!("agent:{}", skill.name),
            "name": skill.name,
            "model": skill.model.clone().unwrap_or_else(|| model.clone()),
            "status": "idle",
            "lastActive": null,
            "platform": "subagent",
        }));
    }
    Ok(Json(serde_json::Value::Array(out)))
}

/// GET /api/agents/skills — loaded skills + agent definitions.
pub async fn skills(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let mut out = Vec::new();
    for s in state.agent.skill_entries().await {
        out.push(json!({
            "name": s.name,
            "description": truncate_chars(&s.description, 160),
            "kind": "skill",
        }));
    }
    for s in state.agent.agent_entries().await {
        out.push(json!({
            "name": s.name,
            "description": truncate_chars(&s.description, 160),
            "kind": "agent",
        }));
    }
    Ok(Json(serde_json::Value::Array(out)))
}

/// POST /api/agents/reload — rescan skills/agents dirs.
pub async fn reload_skills(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let (skills, agents) = state.agent.reload_skills_and_agents().await;
    tracing::info!("Portal: reloaded {skills} skills, {agents} agents");
    Ok(Json(
        json!({ "skillsLoaded": skills, "agentsLoaded": agents }),
    ))
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct MemoryQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// GET /api/memory/search — hybrid knowledge + message search.
pub async fn memory_search(
    State(state): State<PortalState>,
    Query(q): Query<MemoryQuery>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let want_kind = q.kind.as_deref();
    let mut items = Vec::new();

    // Empty query = browse mode (the Memory page loads with q="" on mount;
    // an empty FTS MATCH is a syntax error, so list recent knowledge instead).
    if q.q.trim().is_empty() {
        if matches!(want_kind, None | Some("fact") | Some("knowledge")) {
            let entries = state
                .memory
                .recent_knowledge(limit)
                .await
                .map_err(PortalError::from)?;
            for (rank, e) in entries.into_iter().enumerate() {
                items.push(json!({
                    "id": e.id,
                    "kind": if e.category == "fact" { "fact" } else { "knowledge" },
                    "text": format!("{}: {} — {}", e.category, e.key, e.value),
                    "score": 1.0 - (rank as f64 / (limit.max(1) as f64 + 1.0)),
                    "createdAt": null,
                }));
            }
        }
        return Ok(Json(serde_json::Value::Array(items)));
    }

    if matches!(want_kind, None | Some("fact") | Some("knowledge")) {
        let entries = state
            .memory
            .search_knowledge(&q.q, limit)
            .await
            .map_err(PortalError::from)?;
        for (rank, e) in entries.into_iter().enumerate() {
            items.push(json!({
                "id": e.id,
                "kind": if e.category == "fact" { "fact" } else { "knowledge" },
                "text": format!("{}: {} — {}", e.category, e.key, e.value),
                "score": 1.0 - (rank as f64 / (limit.max(1) as f64 + 1.0)),
                "createdAt": null, // KnowledgeEntry has no timestamp column
            }));
        }
    }
    if matches!(want_kind, None | Some("conversation")) {
        let msgs = state
            .memory
            .search_messages(&q.q, limit)
            .await
            .map_err(PortalError::from)?;
        for (rank, m) in msgs.into_iter().enumerate() {
            let text = m.content.as_ref().map(|c| c.as_text()).unwrap_or_default();
            if text.trim().is_empty() {
                continue;
            }
            items.push(json!({
                "id": format!("msg-{rank}"),
                "kind": "conversation",
                "text": format!("[{}] {}", m.role, truncate_chars(&text, 400)),
                "score": 0.9 - (rank as f64 / (limit.max(1) as f64 + 1.0)),
                "createdAt": null,
            }));
        }
    }
    Ok(Json(serde_json::Value::Array(items)))
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

fn task_json(t: &crate::scheduler::reminders::ScheduledTask) -> serde_json::Value {
    json!({
        "id": t.id,
        "name": if t.description.is_empty() { truncate_chars(&t.prompt, 40) } else { t.description.clone() },
        "cron": if t.trigger_type == "recurring" { t.trigger_value.clone() } else { "once".to_string() },
        "enabled": t.status == "active",
        "nextRun": t.next_run_at.clone().unwrap_or_else(|| "—".to_string()),
        // Full editable state (T3/T5): the edit form needs prompt + trigger
        // verbatim; status distinguishes paused vs completed one-shots.
        "prompt": t.prompt,
        "triggerType": t.trigger_type,
        "triggerValue": t.trigger_value,
        "status": t.status,
        "platform": t.platform,
    })
}

/// GET /api/tasks — every scheduled task (all platforms; Telegram-created
/// tasks remain listed and toggle-able).
pub async fn tasks(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    // Active + paused (completed/cancelled one-shots stay hidden). Paused
    // rows must remain visible so the UI's Enable button can bring them back.
    let active = state
        .task_store
        .list_browsable()
        .await
        .map_err(PortalError::from)?;
    let out: Vec<serde_json::Value> = active.iter().map(task_json).collect();
    Ok(Json(serde_json::Value::Array(out)))
}

#[derive(Deserialize)]
pub struct TaskId {
    pub id: String,
}

/// GET /api/tasks/{id}/runs — last runs for one task.
pub async fn task_runs(
    State(state): State<PortalState>,
    Path(p): Path<TaskId>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let runs = state
        .task_store
        .get_task_runs(&p.id, 20)
        .await
        .map_err(PortalError::from)?;
    let out: Vec<serde_json::Value> = runs
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "runAt": r.run_at,
                "status": r.status,
                "error": r.error.map(|e| truncate_chars(&e, 300)),
                "response": r.response.map(|c| truncate_chars(&c, 400)),
            })
        })
        .collect();
    Ok(Json(serde_json::Value::Array(out)))
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

/// GET /api/health — system vitals from /proc (Linux; best-effort elsewhere).
///
/// **Public** (no auth): the SPA polls it every 15 s and watches `bootId` to
/// detect a process restart, then re-authenticates via its stateless cookie
/// and reconciles chat history (ADR 0008B).
pub async fn health(State(state): State<PortalState>) -> Json<serde_json::Value> {
    let (mut cpu, mut mem_used, mut mem_total, mut disk) = (0.0, 0.0, 0.0, 0.0);
    if let Ok(mem) = tokio::fs::read_to_string("/proc/meminfo").await {
        let mut total_kb = 0u64;
        let mut avail_kb = 0u64;
        for line in mem.lines() {
            if let Some(v) = line.strip_prefix("MemTotal:") {
                total_kb = v.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("MemAvailable:") {
                avail_kb = v.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0);
            }
        }
        mem_total = total_kb as f64 / 1_048_576.0;
        mem_used = (total_kb.saturating_sub(avail_kb)) as f64 / 1_048_576.0;
    }
    if let Ok(stat) = tokio::fs::read_to_string("/proc/stat").await {
        if let Some(line) = stat.lines().find(|l| l.starts_with("cpu ")) {
            let nums: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .filter_map(|v| v.parse().ok())
                .collect();
            if nums.len() >= 4 {
                let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
                let total: u64 = nums.iter().sum();
                cpu = if total > 0 {
                    (1.0 - idle as f64 / total as f64) * 100.0
                } else {
                    0.0
                };
            }
        }
    }
    if let Ok(o) = tokio::process::Command::new("df")
        .arg("-k")
        .arg("/")
        .output()
        .await
    {
        let text = String::from_utf8_lossy(&o.stdout);
        if let Some(line) = text.lines().nth(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(pct) = parts
                .get(4)
                .and_then(|p| p.trim_end_matches('%').parse::<f64>().ok())
            {
                disk = pct;
            }
        }
    }
    Json(json!({
        "cpuPercent": cpu.round(),
        "memUsedGb": (mem_used * 10.0).round() / 10.0,
        "memTotalGb": (mem_total * 10.0).round() / 10.0,
        "diskUsedPercent": disk.round(),
        "uptimeHours": (state.started_at.elapsed().as_secs() as f64 / 3600.0).round(),
        "bootId": state.boot_id,
    }))
}

/// GET /api/stats — counts for the dashboard stat tiles.
pub async fn stats(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let model = state.agent.current_model().await;
    let providers = state.agent.provider_names();
    let conn = state.memory.connection();
    let conn = conn.lock().await;
    let message_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap_or(0);
    let conversation_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap_or(0);
    drop(conn);
    let tasks = state
        .task_store
        .list_all_active()
        .await
        .map_err(PortalError::from)?;
    let skills = state.agent.skill_entries().await.len();
    Ok(Json(json!({
        "model": model,
        "providers": providers,
        "messageCount": message_count,
        "conversationCount": conversation_count,
        "activeTasks": tasks.len(),
        "skills": skills,
        "portalEnabled": true,
    })))
}
