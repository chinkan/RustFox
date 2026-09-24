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
use crate::agent::{parse_one_shot_delay, validate_cron_expr};
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
    match body.trigger_type.as_str() {
        "recurring" => validate_cron_expr(&body.trigger_value)
            .map_err(|e| PortalError::bad_request("invalid_cron", e.to_string()))?,
        "one_shot" => {
            parse_one_shot_delay(&body.trigger_value)
                .map_err(|e| PortalError::bad_request("invalid_trigger", e.to_string()))?;
        }
        other => {
            return Err(PortalError::bad_request(
                "invalid_trigger_type",
                format!("triggerType must be 'recurring' or 'one_shot', got '{other}'"),
            ))
        }
    }

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
        trigger_value: body.trigger_value.clone(),
        prompt: body.prompt.clone(),
        description: name.clone(),
        status: "active".to_string(),
        created_at: now_iso(),
        next_run_at: if body.trigger_type == "one_shot" {
            Some(body.trigger_value.clone())
        } else {
            None
        },
        deleted_at: None,
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
            Ok((
                axum::http::StatusCode::CREATED,
                Json(json!({
                    "id": id,
                    "name": name,
                    "schedulerJobId": job_id.to_string(),
                    "nextRun": task.next_run_at,
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

    if let Some(tt) = &body.trigger_type {
        if tt != &task.trigger_type {
            return Err(PortalError::bad_request(
                "trigger_type_immutable",
                "changing triggerType requires delete + recreate",
            ));
        }
    }

    let new_prompt = body.prompt.clone().filter(|s| !s.trim().is_empty());
    let new_value = body.trigger_value.clone().filter(|s| !s.trim().is_empty());
    let new_name = body.name.clone().filter(|s| !s.trim().is_empty());
    if new_prompt.is_none() && new_value.is_none() && new_name.is_none() {
        return Err(PortalError::bad_request(
            "empty_patch",
            "no editable fields provided (name/prompt/triggerValue)",
        ));
    }
    // fall back to current values for validation of combined state
    let eff_prompt = new_prompt.clone().unwrap_or_else(|| task.prompt.clone());
    let eff_value = new_value
        .clone()
        .unwrap_or_else(|| task.trigger_value.clone());
    let eff_name = new_name.clone().unwrap_or_else(|| task.description.clone());

    if task.trigger_type == "recurring" && body.trigger_value.is_some() {
        validate_cron_expr(&eff_value)
            .map_err(|e| PortalError::bad_request("invalid_cron", e.to_string()))?;
    }
    if task.trigger_type == "one_shot" && body.trigger_value.is_some() {
        parse_one_shot_delay(&eff_value)
            .map_err(|e| PortalError::bad_request("invalid_trigger", e.to_string()))?;
    }

    // next_run_at bookkeeping: one-shots mirror their (new) trigger time;
    // recurring edits clear the discrete value (NULL).
    let next_run = if task.trigger_type == "one_shot" {
        if body.trigger_value.is_some() {
            Some(Some(eff_value.as_str()))
        } else {
            None
        }
    } else if body.trigger_value.is_some() {
        // recurring: no discrete next-run value
        Some(None)
    } else {
        None
    };

    let touched = state
        .task_store
        .update_task_fields(
            &task.id,
            new_prompt.as_deref(),
            new_value.as_deref(),
            new_name.as_deref(),
            next_run,
        )
        .await
        .map_err(PortalError::internal)?;
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

    Ok(Json(json!({
        "id": task.id,
        "updated": {
            "name": eff_name,
            "prompt": eff_prompt,
            "triggerValue": eff_value,
        },
        "rearmed": rearmed.is_some(),
        "schedulerJobId": rearmed,
        "nextRun": if task.trigger_type == "one_shot" { Some(eff_value) } else { None },
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

    state
        .task_store
        .set_status(&p.id, "active")
        .await
        .map_err(PortalError::internal)?;
    let mut armed = task.clone();
    armed.status = "active".to_string();
    // A one-shot whose time already passed cannot be re-armed meaningfully.
    if armed.trigger_type == "one_shot" && parse_one_shot_delay(&armed.trigger_value).is_err() {
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
            Ok(Json(json!({
            "ok": true,
            "id": p.id,
            "enabled": true,
            "schedulerJobId": job.to_string(),
            "nextRun": if task.trigger_type == "one_shot" { Some(task.trigger_value.clone()) } else { None },
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
