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
pub async fn agents(State(state): State<PortalState>) -> Result<Json<serde_json::Value>, PortalError> {
    let model = state.agent.current_model.read().await.clone();
    let web_active = state.agent.is_processing(&state.config.user_name).await;
    let mut telegram_active = false;
    for id in &state.agent.config.telegram.allowed_user_ids {
        if state.agent.is_processing(&id.to_string()).await {
            telegram_active = true;
            break;
        }
    }

    let mut out = Vec::new();
    let status = if web_active || telegram_active { "running" } else { "idle" };
    out.push(json!({
        "id": "main",
        "name": "rustfox",
        "model": model,
        "status": status,
        "lastActive": chrono::Utc::now().to_rfc3339(),
        "platform": "telegram+web",
    }));

    {
        let agents = state.agent.agents.read().await;
        for skill in agents.list() {
            out.push(json!({
                "id": format!("agent:{}", skill.name),
                "name": skill.name,
                "model": skill.model.clone().unwrap_or_else(|| model.clone()),
                "status": "idle",
                "lastActive": null,
                "platform": "subagent",
            }));
        }
    }
    Ok(Json(serde_json::Value::Array(out)))
}

/// GET /api/agents/skills — loaded skills + agent definitions.
pub async fn skills(State(state): State<PortalState>) -> Result<Json<serde_json::Value>, PortalError> {
    let mut out = Vec::new();
    {
        let skills = state.agent.skills.read().await;
        for s in skills.list() {
            out.push(json!({
                "name": s.name,
                "description": truncate_chars(&s.description, 160),
                "kind": "skill",
            }));
        }
    }
    {
        let agents = state.agent.agents.read().await;
        for s in agents.list() {
            out.push(json!({
                "name": s.name,
                "description": truncate_chars(&s.description, 160),
                "kind": "agent",
            }));
        }
    }
    Ok(Json(serde_json::Value::Array(out)))
}

/// POST /api/agents/reload — rescan skills/agents dirs.
pub async fn reload_skills(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let (skills, agents) = state.agent.reload_skills_and_agents().await;
    tracing::info!("Portal: reloaded {skills} skills, {agents} agents");
    Ok(Json(json!({ "skillsLoaded": skills, "agentsLoaded": agents })))
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
    })
}

/// GET /api/tasks — every scheduled task (all platforms; Telegram-created
/// tasks remain listed and toggle-able).
pub async fn tasks(State(state): State<PortalState>) -> Result<Json<serde_json::Value>, PortalError> {
    // list_all_active only returns active; fetch all via the store's SQL when
    // possible, else merge active + recent runs. MVP: active list + status map.
    let active = state.task_store.list_all_active().await.map_err(PortalError::from)?;
    // MVP: enumerate the active set. Paused/one-shot-completed tasks need a
    // `list_all_including_disabled` store method (follow-up).
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

/// POST /api/tasks/{id}/enable — set status active and re-register the job.
pub async fn task_enable(
    State(state): State<PortalState>,
    Path(p): Path<TaskId>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let task = state
        .task_store
        .get_by_id(&p.id)
        .await
        .map_err(PortalError::from)?
        .ok_or_else(|| PortalError::not_found("task"))?;
    state
        .task_store
        .set_status(&p.id, "active")
        .await
        .map_err(PortalError::from)?;
    // Re-arming the live cron job from an Arc<Agent> needs the Telegram bot
    // handle (fire closure captures it), which restore_scheduled_tasks owns.
    // MVP behaviour: status flip takes effect on next restart; the UI marks it.
    let _ = &task;
    Ok(Json(json!({ "ok": true, "id": p.id, "enabled": true, "restartToSchedule": true })))
}

/// POST /api/tasks/{id}/disable — pause scheduling + mark inactive.
pub async fn task_disable(
    State(state): State<PortalState>,
    Path(p): Path<TaskId>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let task = state
        .task_store
        .get_by_id(&p.id)
        .await
        .map_err(PortalError::from)?
        .ok_or_else(|| PortalError::not_found("task"))?;
    if let Some(job_id) = task.scheduler_job_id.as_deref() {
        if let Ok(uuid) = job_id.parse::<uuid::Uuid>() {
            let _ = state.agent.scheduler.remove_job(uuid).await;
        }
    }
    state
        .task_store
        .set_status(&p.id, "paused")
        .await
        .map_err(PortalError::from)?;
    Ok(Json(json!({ "ok": true, "id": p.id, "enabled": false })))
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

/// GET /api/health — system vitals from /proc (Linux; best-effort elsewhere).
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
    if let Ok(o) = tokio::process::Command::new("df").arg("-k").arg("/").output().await {
        let text = String::from_utf8_lossy(&o.stdout);
        if let Some(line) = text.lines().nth(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(pct) = parts.get(4).and_then(|p| p.trim_end_matches('%').parse::<f64>().ok()) {
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
    }))
}

/// GET /api/stats — counts for the dashboard stat tiles.
pub async fn stats(State(state): State<PortalState>) -> Result<Json<serde_json::Value>, PortalError> {
    let model = state.agent.current_model.read().await.clone();
    let providers = state.agent.registry.provider_names();
    let conn = state.memory.connection();
    let conn = conn.lock().await;
    let message_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap_or(0);
    let conversation_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap_or(0);
    drop(conn);
    let tasks = state.task_store.list_all_active().await.map_err(PortalError::from)?;
    let skills = state.agent.skills.read().await.list().len();
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
