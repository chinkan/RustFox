//! Task CRUD handlers (T3, ADR-0011a R6).
//!
//! The portal's write surface for scheduled tasks: create, edit, delete
//! (soft), enable/disable — all routed through `AgentOps::arm_task` /
//! `disarm_task` so the DB row and the live JobScheduler never diverge
//! (the old `enable` endpoint just flipped status and told the UI to
//! restart — `restartToSchedule` is gone).
//!
//! Semantics locked with Kan (Round 3 Q10):
//! - `triggerType` is IMMUTABLE after creation (cron ⇄ one-shot is a
//!   different mechanism; delete + recreate instead).
//! - Editing a live task re-arms it immediately; an in-flight run finishes
//!   untouched (tokio spawns are naturally immune to job removal).
//! - DELETE is a soft delete: row + ALL run history survive (evidence, R6).

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::error::PortalError;
use super::PortalState;
use crate::agent::{normalize_cron_expr, parse_one_shot_delay};
use crate::scheduler::reminders::ScheduledTask;

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

#[derive(Deserialize)]
pub struct CreateTaskBody {
    /// Human-readable label (maps to DB `description`).
    #[serde(default, alias = "description")]
    name: Option<String>,
    prompt: String,
    /// "recurring" | "one_shot"
    #[serde(rename = "triggerType")]
    trigger_type: String,
    /// 6-field cron (recurring) or ISO-8601 local datetime (one_shot).
    #[serde(rename = "triggerValue")]
    trigger_value: String,
    /// Optional IANA timezone; missing → effective default (ADR-0022).
    #[serde(default)]
    timezone: Option<String>,
    /// Glob under the RustFox home. Missing or blank → compat completion path.
    #[serde(default, rename = "expectedArtifact")]
    expected_artifact: Option<String>,
    /// Lookback seconds. Missing → 900. Values below 300 are rejected.
    #[serde(default, rename = "artifactGraceSecs")]
    artifact_grace_secs: Option<i64>,
}

/// Missing field stays `None` via `#[serde(default)]`. JSON null becomes
/// `Some(None)` so an update can clear `expected_artifact`.
fn deserialize_some<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn parse_expected_artifact(raw: &str) -> Result<Option<String>, PortalError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    crate::scheduler::completion::validate_artifact_glob(trimmed)
        .map_err(|e| PortalError::bad_request("invalid_artifact_glob", e))?;
    Ok(Some(trimmed.to_string()))
}

fn parse_artifact_grace(secs: i64) -> Result<i64, PortalError> {
    crate::scheduler::completion::validate_artifact_grace(secs)
        .map_err(|e| PortalError::bad_request("invalid_artifact_grace", e))?;
    Ok(secs)
}

fn portal_owns(state: &PortalState, task: &ScheduledTask) -> Result<(), PortalError> {
    if task.bot_id == state.bot_id() {
        Ok(())
    } else {
        Err(PortalError::not_found("task"))
    }
}

/// POST /api/tasks — create + arm immediately.
pub async fn task_create(
    State(state): State<PortalState>,
    Json(body): Json<CreateTaskBody>,
) -> Result<(axum::http::StatusCode, Json<Value>), PortalError> {
    let name = body.name.clone().unwrap_or_default();
    if body.prompt.trim().is_empty() {
        return Err(PortalError::bad_request(
            "empty_prompt",
            "prompt must not be empty",
        ));
    }
    if name.trim().is_empty() {
        return Err(PortalError::bad_request(
            "empty_name",
            "name (description) must not be empty",
        ));
    }
    let default_tz = state.schedule_tz.tz;
    let tz_stored = match body
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => default_tz.name().to_string(),
        Some(name) => crate::scheduler::timezone::parse_iana(name, "timezone")
            .map_err(|e| PortalError::bad_request("invalid_timezone", e.to_string()))?
            .name()
            .to_string(),
    };
    let tz = crate::scheduler::timezone::parse_task_tz(&tz_stored)
        .map_err(|e| PortalError::bad_request("invalid_timezone", e.to_string()))?;

    let mut trigger_value = body.trigger_value.clone();
    match body.trigger_type.as_str() {
        "recurring" => {
            trigger_value = normalize_cron_expr(&body.trigger_value)
                .map_err(|e| PortalError::bad_request("invalid_cron", e.to_string()))?
        }
        "one_shot" => {
            parse_one_shot_delay(&body.trigger_value, tz)
                .map_err(|e| PortalError::bad_request("invalid_trigger", e.to_string()))?;
        }
        other => {
            return Err(PortalError::bad_request(
                "invalid_trigger_type",
                format!("triggerType must be 'recurring' or 'one_shot', got '{other}'"),
            ))
        }
    }

    let expected_artifact = match body.expected_artifact.as_deref() {
        Some(raw) => parse_expected_artifact(raw)?,
        None => None,
    };
    let artifact_grace_secs = match body.artifact_grace_secs {
        Some(n) => parse_artifact_grace(n)?,
        None => crate::scheduler::completion::DEFAULT_ARTIFACT_GRACE_SECS,
    };

    let id = uuid::Uuid::new_v4().to_string();
    let task = ScheduledTask {
        id: id.clone(),
        scheduler_job_id: None,
        // Portal-created tasks belong to the owner's Telegram identity —
        // the same user the bot serves. chat_id comes from config when set;
        // otherwise the platform-native runner still delivers the agent's
        // response to the owner's chat via `sender`.
        user_id: state.config.user_name.clone(),
        chat_id: state
            .agent
            .config()
            .telegram
            .allowed_user_ids
            .first()
            .map(|u| u.to_string())
            .unwrap_or_default(),
        platform: "portal".to_string(),
        trigger_type: body.trigger_type.clone(),
        trigger_value: trigger_value.clone(),
        prompt: body.prompt.clone(),
        description: name.clone(),
        status: "active".to_string(),
        bot_id: state.bot_id(),
        created_at: now_iso(),
        next_run_at: if body.trigger_type == "one_shot" {
            Some(body.trigger_value.clone())
        } else {
            None
        },
        deleted_at: None,
        timezone: tz_stored.clone(),
        expected_artifact,
        artifact_grace_secs,
    };

    state
        .task_store
        .create(&task)
        .await
        .map_err(PortalError::internal)?;

    match state.agent.arm_task(task.clone()).await {
        Ok(job_id) => {
            // Persist the live job id (the AgentOps contract only promises a
            // successful arm; handlers own the DB write-back so fakes and the
            // real agent behave identically for disable/delete lookups).
            let _ = state
                .task_store
                .update_scheduler_job_id(&id, &job_id.to_string())
                .await;
            // Issue #111 (Part A): `arm_task` refreshed `next_run_at` to the
            // true next fire instant — read it back so the response doesn't
            // repeat the stale (NULL-for-recurring) in-memory value.
            let next_run = state
                .task_store
                .get_by_id(&id)
                .await
                .ok()
                .flatten()
                .and_then(|t| t.next_run_at);
            Ok((
                axum::http::StatusCode::CREATED,
                Json(json!({
                    "id": id,
                    "name": name,
                    "triggerValue": trigger_value,
                    "timezone": tz_stored,
                    "schedulerJobId": job_id.to_string(),
                    "nextRun": next_run,
                })),
            ))
        }
        Err(e) => {
            // Roll the row back: a task that exists in the DB but never
            // armed would resurrect silently on next restart — worse than
            // failing loudly here.
            let _ = state.task_store.soft_delete(&id).await;
            Err(PortalError::bad_request(
                "arm_failed",
                format!("Task saved but could not be scheduled: {e}"),
            ))
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateTaskBody {
    #[serde(default, alias = "description")]
    name: Option<String>,
    prompt: Option<String>,
    #[serde(rename = "triggerValue")]
    trigger_value: Option<String>,
    /// Rejected outright if present and different from the stored type.
    #[serde(rename = "triggerType")]
    trigger_type: Option<String>,
    /// Optional IANA timezone (ADR-0022).
    #[serde(default)]
    timezone: Option<String>,
    /// `null` or blank clears the glob. Absent leaves it.
    /// `deserialize_some` is required: serde's `Option` turns JSON null into
    /// `None` before the inner `Option` can see it, so a plain
    /// `Option<Option<String>>` cannot tell "clear" from "field omitted".
    #[serde(
        default,
        rename = "expectedArtifact",
        deserialize_with = "deserialize_some"
    )]
    expected_artifact: Option<Option<String>>,
    /// Absent leaves the stored grace. Below 300 is rejected.
    #[serde(default, rename = "artifactGraceSecs")]
    artifact_grace_secs: Option<i64>,
}

/// PUT /api/tasks/{id} — edit + re-arm (if live). Partial update.
pub async fn task_update(
    State(state): State<PortalState>,
    Path(p): Path<super::data::TaskId>,
    Json(body): Json<UpdateTaskBody>,
) -> Result<Json<Value>, PortalError> {
    let task = state
        .task_store
        .get_by_id(&p.id)
        .await
        .map_err(PortalError::internal)?
        .filter(|t| t.deleted_at.is_none())
        .ok_or_else(|| PortalError::not_found("task"))?;
    portal_owns(&state, &task)?;

    if let Some(tt) = &body.trigger_type {
        if tt != &task.trigger_type {
            return Err(PortalError::bad_request(
                "trigger_type_immutable",
                "changing triggerType requires delete + recreate",
            ));
        }
    }

    let new_prompt = body.prompt.clone().filter(|s| !s.trim().is_empty());
    let mut new_value = body.trigger_value.clone().filter(|s| !s.trim().is_empty());
    let new_name = body.name.clone().filter(|s| !s.trim().is_empty());
    let new_tz = body
        .timezone
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(ref name) = new_tz {
        crate::scheduler::timezone::parse_iana(name, "timezone")
            .map_err(|e| PortalError::bad_request("invalid_timezone", e.to_string()))?;
    }
    let artifact_write = match &body.expected_artifact {
        None => None,
        Some(inner) => Some(match inner.as_deref() {
            Some(raw) => parse_expected_artifact(raw)?,
            None => None,
        }),
    };
    let grace_write = match body.artifact_grace_secs {
        Some(n) => Some(parse_artifact_grace(n)?),
        None => None,
    };
    if new_prompt.is_none()
        && new_value.is_none()
        && new_name.is_none()
        && new_tz.is_none()
        && artifact_write.is_none()
        && grace_write.is_none()
    {
        return Err(PortalError::bad_request(
            "empty_patch",
            "no editable fields provided (name/prompt/triggerValue/timezone/expectedArtifact/artifactGraceSecs)",
        ));
    }
    // fall back to current values for validation of combined state
    let eff_prompt = new_prompt.clone().unwrap_or_else(|| task.prompt.clone());
    let mut eff_value = new_value
        .clone()
        .unwrap_or_else(|| task.trigger_value.clone());
    let eff_name = new_name.clone().unwrap_or_else(|| task.description.clone());
    let eff_tz_name = new_tz.clone().unwrap_or_else(|| task.timezone.clone());
    let eff_tz = crate::scheduler::timezone::parse_task_tz(&eff_tz_name)
        .map_err(|e| PortalError::bad_request("invalid_timezone", e.to_string()))?;

    if task.trigger_type == "recurring" && body.trigger_value.is_some() {
        eff_value = normalize_cron_expr(&eff_value)
            .map_err(|e| PortalError::bad_request("invalid_cron", e.to_string()))?;
        new_value = Some(eff_value.clone());
    }
    if task.trigger_type == "one_shot" && (body.trigger_value.is_some() || new_tz.is_some()) {
        parse_one_shot_delay(&eff_value, eff_tz)
            .map_err(|e| PortalError::bad_request("invalid_trigger", e.to_string()))?;
    }

    // `next_run_at` is left untouched here (issue #111, Part A): the handler
    // used to NULL it for recurring edits and mirror the raw trigger for
    // one-shots. We now recompute it from the trigger below (via arm on the
    // active path, or an explicit refresh otherwise) so the stored value is
    // always the real next fire instant.
    let mut touched = 0;
    if new_prompt.is_some() || new_value.is_some() || new_name.is_some() || new_tz.is_some() {
        touched = state
            .task_store
            .update_task_fields(
                &task.id,
                new_prompt.as_deref(),
                new_value.as_deref(),
                new_name.as_deref(),
                None,
                new_tz.as_deref(),
            )
            .await
            .map_err(PortalError::internal)?;
    }
    if artifact_write.is_some() || grace_write.is_some() {
        let n = state
            .task_store
            .update_artifact_expectation(
                &task.id,
                artifact_write.as_ref().map(|inner| inner.as_deref()),
                grace_write,
            )
            .await
            .map_err(PortalError::internal)?;
        touched += n;
    }
    if touched == 0 {
        return Err(PortalError::not_found("task"));
    }

    // Re-arm iff the task is live (active). A paused task's edit is saved
    // but stays un-armed; enable will arm with the new fields.
    let rearmed = if task.status == "active" {
        // disarm old job (idempotent), refetch updated row, arm it
        let _ = state.agent.disarm_task(task.clone()).await;
        let updated = state
            .task_store
            .get_by_id(&task.id)
            .await
            .map_err(PortalError::internal)?
            .ok_or_else(|| PortalError::not_found("task"))?;
        match state.agent.arm_task(updated.clone()).await {
            Ok(job) => {
                let _ = state
                    .task_store
                    .update_scheduler_job_id(&updated.id, &job.to_string())
                    .await;
                Some(job.to_string())
            }
            Err(e) => {
                // Row is updated but the job failed to arm (e.g. cron expr
                // the JobScheduler rejects beyond our 6-field check).
                // Surface it — the UI must know it's not actually running.
                return Err(PortalError::bad_request(
                    "arm_failed",
                    format!("Task updated but re-arm failed: {e}"),
                ));
            }
        }
    } else {
        None
    };

    // Recompute the authoritative next fire instant. On the active path
    // `arm_task` already refreshed it; this covers the paused path (edit saved
    // but not armed) and is idempotent otherwise. Then read the row back so the
    // response reflects one consistent source of truth.
    let _ = state.task_store.refresh_next_run_at(&task.id).await;
    let next_run = state
        .task_store
        .get_by_id(&task.id)
        .await
        .ok()
        .flatten()
        .and_then(|t| t.next_run_at);

    Ok(Json(json!({
        "id": task.id,
        "updated": {
            "name": eff_name,
            "prompt": eff_prompt,
            "triggerValue": eff_value,
            "timezone": eff_tz_name,
        },
        "rearmed": rearmed.is_some(),
        "schedulerJobId": rearmed,
        "nextRun": next_run,
    })))
}

/// DELETE /api/tasks/{id} — disarm + soft delete (R6: history survives).
pub async fn task_delete(
    State(state): State<PortalState>,
    Path(p): Path<super::data::TaskId>,
) -> Result<Json<Value>, PortalError> {
    let task = state
        .task_store
        .get_by_id(&p.id)
        .await
        .map_err(PortalError::internal)?
        .filter(|t| t.deleted_at.is_none())
        .ok_or_else(|| PortalError::not_found("task"))?;
    portal_owns(&state, &task)?;

    let _ = state.agent.disarm_task(task.clone()).await;
    let n = state
        .task_store
        .soft_delete(&task.id)
        .await
        .map_err(PortalError::internal)?;
    if n == 0 {
        return Err(PortalError::not_found("task"));
    }
    Ok(Json(json!({
        "ok": true,
        "id": task.id,
        "softDeleted": true,
        "historyPreserved": true,
    })))
}

/// POST /api/tasks/{id}/enable — real re-arm (the restartToSchedule fib dies here).
pub async fn task_enable(
    State(state): State<PortalState>,
    Path(p): Path<super::data::TaskId>,
) -> Result<Json<Value>, PortalError> {
    let task = state
        .task_store
        .get_by_id(&p.id)
        .await
        .map_err(PortalError::internal)?
        .filter(|t| t.deleted_at.is_none())
        .ok_or_else(|| PortalError::not_found("task"))?;
    portal_owns(&state, &task)?;

    state
        .task_store
        .set_status(&p.id, "active")
        .await
        .map_err(PortalError::internal)?;
    let mut armed = task.clone();
    armed.status = "active".to_string();
    // A one-shot whose time already passed cannot be re-armed meaningfully.
    let armed_tz =
        crate::scheduler::timezone::parse_task_tz(&armed.timezone).unwrap_or(chrono_tz::UTC);
    if armed.trigger_type == "one_shot"
        && parse_one_shot_delay(&armed.trigger_value, armed_tz).is_err()
    {
        let _ = state.task_store.set_status(&p.id, &task.status).await;
        return Err(PortalError::bad_request(
            "trigger_passed",
            format!(
                "one-shot trigger {} is in the past — edit the time first",
                armed.trigger_value
            ),
        ));
    }
    match state.agent.arm_task(armed.clone()).await {
        Ok(job) => {
            let _ = state
                .task_store
                .update_scheduler_job_id(&armed.id, &job.to_string())
                .await;
            // Issue #111 (Part A): reflect the arm-refreshed next fire instant.
            let next_run = state
                .task_store
                .get_by_id(&p.id)
                .await
                .ok()
                .flatten()
                .and_then(|t| t.next_run_at);
            Ok(Json(json!({
            "ok": true,
            "id": p.id,
            "enabled": true,
            "schedulerJobId": job.to_string(),
            "nextRun": next_run,
            })))
        }
        Err(e) => {
            let _ = state.task_store.set_status(&p.id, &task.status).await;
            Err(PortalError::bad_request(
                "arm_failed",
                format!("could not re-schedule task: {e}"),
            ))
        }
    }
}

/// POST /api/tasks/{id}/disable — disarm live job + status → paused.
pub async fn task_disable(
    State(state): State<PortalState>,
    Path(p): Path<super::data::TaskId>,
) -> Result<Json<Value>, PortalError> {
    let task = state
        .task_store
        .get_by_id(&p.id)
        .await
        .map_err(PortalError::internal)?
        .filter(|t| t.deleted_at.is_none())
        .ok_or_else(|| PortalError::not_found("task"))?;
    portal_owns(&state, &task)?;

    let removed = state.agent.disarm_task(task.clone()).await;
    state
        .task_store
        .set_status(&p.id, "paused")
        .await
        .map_err(PortalError::internal)?;
    Ok(Json(json!({
        "ok": true,
        "id": p.id,
        "enabled": false,
        "jobRemoved": removed,
    })))
}
