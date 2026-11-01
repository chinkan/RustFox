//! Same-day progress for one scheduled task (#331 / ADR-0054).
//!
//! File: `{workspace}/_runs/{task_id}-YYYY-MM-DD.json`. The date is the
//! task timezone's calendar day (ADR-0022). A later fresh run of the same
//! task on that day is told to continue from `next_phase` and not redo
//! completed work. Success and an explicit hard failure delete the file.
//!
//! This is not the SQLite transcript checkpoint (ADR-0039) and it does not
//! read `.plans/` (issue C is out of scope). It does not change
//! `max_iterations`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_EVIDENCE: usize = 20;
const MAX_EVIDENCE_CHARS: usize = 400;
const MAX_PHASE_CHARS: usize = 200;

/// How a scheduled run ended, for the daily file and the transcript checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    /// Wall-clock timeout (ADR-0024).
    Timeout,
    /// Agent loop exhausted `max_iterations`.
    MaxIterations,
    /// A guard replaced the final reply and the run was not verified.
    GuardRewrite,
    /// Missing artifact or short clarify/menu. Resumable file, no checkpoint.
    UnverifiedOther,
    /// [`crate::scheduler::completion::ScheduleCompletion::Completed`].
    Completed,
    /// Hard agent error that will not auto re-fire (including failed-twice).
    ExplicitFailure,
    /// Transient provider error. The hourly re-fire should keep any file the
    /// agent already wrote, and should not invent one from the 429 alone.
    TransientFailure,
}

/// What to do with today's progress file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DailyFileAction {
    /// Remove today's file. Missing is success.
    Delete,
    /// Create or overwrite today's file, keeping phases already on disk.
    Upsert { evidence: String },
    /// Do not create, update, or delete.
    Leave,
}

/// Policy for one scheduled stop. The runner applies `daily` and, when
/// `save_transcript` is set, calls `CheckpointStore::save` on the human-gate
/// queue id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossRunPlan {
    pub daily: DailyFileAction,
    pub save_transcript: bool,
}

/// `workspace/_runs/<task_id>-YYYY-MM-DD.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyRunProgress {
    pub task_id: String,
    pub date: String,
    #[serde(default)]
    pub last_phase: Option<String>,
    /// Skill-written files may use `next` instead of `next_phase`.
    #[serde(default, alias = "next")]
    pub next_phase: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl DailyRunProgress {
    fn fresh(task_id: &str, date: &str) -> Self {
        Self {
            task_id: task_id.to_string(),
            date: date.to_string(),
            last_phase: None,
            next_phase: None,
            evidence: Vec::new(),
            attempt: 0,
            updated_at: None,
        }
    }
}

/// Decide the daily file and whether this stop writes a transcript checkpoint.
///
/// Timeout, max-iterations, and guard rewrite save a checkpoint (ADR-0039
/// amended by ADR-0054). A clean completion and an explicit hard failure
/// delete the daily file. A transient provider error leaves the file alone.
pub fn plan_cross_run(kind: StopKind, evidence: impl Into<String>) -> CrossRunPlan {
    let evidence = evidence.into();
    match kind {
        StopKind::Timeout | StopKind::MaxIterations | StopKind::GuardRewrite => CrossRunPlan {
            daily: DailyFileAction::Upsert { evidence },
            save_transcript: true,
        },
        StopKind::UnverifiedOther => CrossRunPlan {
            daily: DailyFileAction::Upsert { evidence },
            save_transcript: false,
        },
        StopKind::Completed | StopKind::ExplicitFailure => CrossRunPlan {
            daily: DailyFileAction::Delete,
            save_transcript: false,
        },
        StopKind::TransientFailure => CrossRunPlan {
            daily: DailyFileAction::Leave,
            save_transcript: false,
        },
    }
}

/// Calendar day `YYYY-MM-DD` in `tz_name`. Unknown zones use the UTC day.
pub fn local_calendar_date(tz_name: &str, now: DateTime<Utc>) -> String {
    match crate::scheduler::timezone::parse_task_tz(tz_name) {
        Ok(tz) => now.with_timezone(&tz).format("%Y-%m-%d").to_string(),
        Err(_) => now.format("%Y-%m-%d").to_string(),
    }
}

/// Load today's file. `Ok(None)` when it is absent. Invalid JSON is an error
/// so a fresh run does not invent a continue instruction from garbage.
pub fn load(workspace: &Path, task_id: &str, date: &str) -> Result<Option<DailyRunProgress>> {
    let path = progress_path(workspace, task_id, date)?;
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read daily run progress {}", path.display()))?;
    let progress = serde_json::from_str(&raw)
        .with_context(|| format!("parse daily run progress {}", path.display()))?;
    Ok(Some(progress))
}

/// Apply [`plan_cross_run`]'s file action.
pub fn apply(workspace: &Path, task_id: &str, date: &str, action: DailyFileAction) -> Result<()> {
    match action {
        DailyFileAction::Leave => Ok(()),
        DailyFileAction::Delete => {
            delete(workspace, task_id, date)?;
            Ok(())
        }
        DailyFileAction::Upsert { evidence } => upsert(workspace, task_id, date, &evidence),
    }
}

/// Append the continue instruction. The original scheduled text, including a
/// leading `[schedule:…]` marker, stays intact.
pub fn inject_instruction(scheduled_text: &str, progress: &DailyRunProgress) -> String {
    let block = continue_instruction(progress);
    if scheduled_text.is_empty() {
        block
    } else {
        format!("{scheduled_text}\n\n{block}")
    }
}

fn continue_instruction(progress: &DailyRunProgress) -> String {
    let next = match phase_line(progress.next_phase.as_deref()) {
        Some(phase) => format!("Continue from next_phase: {phase}."),
        None => "next_phase was not recorded; continue from the evidence and do not restart from scratch.".to_string(),
    };
    let last =
        phase_line(progress.last_phase.as_deref()).unwrap_or_else(|| "(none recorded)".to_string());
    let evidence = if progress.evidence.is_empty() {
        "(none)".to_string()
    } else {
        progress
            .evidence
            .iter()
            .map(|line| format!("- {}", one_line(line)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "[run-progress] A previous run of this same task on {date} already made progress (attempt {attempt}). Last completed phase: {last}. {next} Do not redo completed work.\nEvidence:\n{evidence}",
        date = progress.date,
        attempt = progress.attempt,
    )
}

fn upsert(workspace: &Path, task_id: &str, date: &str, evidence: &str) -> Result<()> {
    let mut progress = match load(workspace, task_id, date) {
        Ok(Some(existing)) => existing,
        Ok(None) => DailyRunProgress::fresh(task_id, date),
        Err(e) => {
            let path = progress_path(workspace, task_id, date)?;
            if path.is_file() {
                tracing::warn!("Replacing unreadable daily run progress for {task_id}: {e:#}");
                DailyRunProgress::fresh(task_id, date)
            } else {
                return Err(e);
            }
        }
    };
    progress.task_id = task_id.to_string();
    progress.date = date.to_string();
    progress.attempt = progress.attempt.saturating_add(1);
    let line = one_line(evidence);
    if !line.is_empty() {
        progress
            .evidence
            .push(clip_chars(&line, MAX_EVIDENCE_CHARS));
        if progress.evidence.len() > MAX_EVIDENCE {
            let drop_n = progress.evidence.len() - MAX_EVIDENCE;
            progress.evidence.drain(0..drop_n);
        }
    }
    progress.updated_at = Some(Utc::now().to_rfc3339());
    write_progress(workspace, &progress)
}

fn delete(workspace: &Path, task_id: &str, date: &str) -> Result<bool> {
    let path = progress_path(workspace, task_id, date)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("delete daily run progress {}", path.display())),
    }
}

fn write_progress(workspace: &Path, progress: &DailyRunProgress) -> Result<()> {
    let path = progress_path(workspace, &progress.task_id, &progress.date)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(progress).context("serialize daily run progress")?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("daily run progress file name")?;
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&tmp, format!("{json}\n"))
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename daily run progress {}", path.display()))?;
    Ok(())
}

fn progress_path(workspace: &Path, task_id: &str, date: &str) -> Result<PathBuf> {
    validate_task_id(task_id)?;
    validate_date(date)?;
    Ok(workspace
        .join("_runs")
        .join(format!("{task_id}-{date}.json")))
}

fn validate_task_id(task_id: &str) -> Result<()> {
    if task_id.is_empty() || task_id.len() > 200 {
        anyhow::bail!("daily run progress task id is empty or too long");
    }
    if !task_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("daily run progress task id has unsafe characters");
    }
    Ok(())
}

fn validate_date(date: &str) -> Result<()> {
    let b = date.as_bytes();
    let ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !ok {
        anyhow::bail!("daily run progress date must be YYYY-MM-DD");
    }
    Ok(())
}

fn phase_line(raw: Option<&str>) -> Option<String> {
    let line = one_line(raw.unwrap_or(""));
    if line.is_empty() {
        None
    } else {
        Some(clip_chars(&line, MAX_PHASE_CHARS))
    }
}

fn one_line(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip_chars(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn plan_checkpoints_timeout_max_iterations_and_guard_only() {
        for kind in [
            StopKind::Timeout,
            StopKind::MaxIterations,
            StopKind::GuardRewrite,
        ] {
            let plan = plan_cross_run(kind, "evidence");
            assert!(plan.save_transcript, "{kind:?}");
            assert!(
                matches!(plan.daily, DailyFileAction::Upsert { .. }),
                "{kind:?}"
            );
        }
        let other = plan_cross_run(StopKind::UnverifiedOther, "missing artifact");
        assert!(!other.save_transcript);
        assert!(matches!(other.daily, DailyFileAction::Upsert { .. }));
        assert!(!plan_cross_run(StopKind::Completed, "").save_transcript);
        assert!(matches!(
            plan_cross_run(StopKind::Completed, "").daily,
            DailyFileAction::Delete
        ));
        assert!(matches!(
            plan_cross_run(StopKind::ExplicitFailure, "boom").daily,
            DailyFileAction::Delete
        ));
        assert!(matches!(
            plan_cross_run(StopKind::TransientFailure, "429").daily,
            DailyFileAction::Leave
        ));
        assert!(!plan_cross_run(StopKind::TransientFailure, "429").save_transcript);
    }

    #[test]
    fn calendar_date_uses_task_timezone() {
        let now = Utc.with_ymd_and_hms(2026, 10, 31, 16, 30, 0).unwrap();
        assert_eq!(local_calendar_date("Asia/Hong_Kong", now), "2026-11-01");
        assert_eq!(local_calendar_date("UTC", now), "2026-10-31");
        assert_eq!(local_calendar_date("Not/AZone", now), "2026-10-31");
    }

    #[test]
    fn skill_next_alias_injects_continue_and_forbids_redo() {
        let dir = ws();
        let raw = r#"{"task_id":"dream-cycle","date":"2026-10-31","last_phase":"H1-H2","next":"M1b","attempt":2,"evidence":["memory snapshot done"]}"#;
        let runs = dir.path().join("_runs");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(runs.join("dream-cycle-2026-10-31.json"), raw).unwrap();

        let loaded = load(dir.path(), "dream-cycle", "2026-10-31")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.next_phase.as_deref(), Some("M1b"));
        assert!(load(dir.path(), "dream-cycle", "2026-11-01")
            .unwrap()
            .is_none());

        let text = inject_instruction("[schedule:abc]\nrun dream", &loaded);
        assert!(text.starts_with("[schedule:abc]\nrun dream\n"));
        assert!(text.contains("Continue from next_phase: M1b."));
        assert!(text.contains("Do not redo completed work"));
        assert!(text.contains("H1-H2"));
        assert!(text.contains("memory snapshot done"));
    }

    #[test]
    fn resumable_stop_keeps_phases_and_terminal_stops_delete() {
        let dir = ws();
        let raw = r#"{"task_id":"task-1","date":"2026-11-01","last_phase":"W2","next_phase":"D1","attempt":1,"evidence":["step 1 done"]}"#;
        let runs = dir.path().join("_runs");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(runs.join("task-1-2026-11-01.json"), raw).unwrap();

        let plan = plan_cross_run(StopKind::MaxIterations, "Reached max iterations (25)");
        apply(dir.path(), "task-1", "2026-11-01", plan.daily).unwrap();
        let kept = load(dir.path(), "task-1", "2026-11-01").unwrap().unwrap();
        assert_eq!(kept.next_phase.as_deref(), Some("D1"));
        assert_eq!(kept.last_phase.as_deref(), Some("W2"));
        assert_eq!(kept.attempt, 2);
        assert_eq!(
            kept.evidence,
            vec![
                "step 1 done".to_string(),
                "Reached max iterations (25)".to_string()
            ]
        );
        assert!(kept.updated_at.is_some());
        assert!(runs.join("task-1-2026-11-01.json").is_file());

        apply(
            dir.path(),
            "task-1",
            "2026-11-01",
            plan_cross_run(StopKind::TransientFailure, "429").daily,
        )
        .unwrap();
        assert_eq!(
            load(dir.path(), "task-1", "2026-11-01")
                .unwrap()
                .unwrap()
                .attempt,
            2
        );

        apply(
            dir.path(),
            "task-1",
            "2026-11-01",
            plan_cross_run(StopKind::Completed, "").daily,
        )
        .unwrap();
        assert!(load(dir.path(), "task-1", "2026-11-01").unwrap().is_none());

        apply(
            dir.path(),
            "task-1",
            "2026-11-01",
            plan_cross_run(StopKind::ExplicitFailure, "boom").daily,
        )
        .unwrap();
        assert!(load(dir.path(), "task-1", "2026-11-01").unwrap().is_none());
    }

    #[test]
    fn first_resumable_stop_creates_file_without_a_phase() {
        let dir = ws();
        let plan = plan_cross_run(StopKind::Timeout, "wall-clock timeout after 900s");
        apply(dir.path(), "task_1", "2026-11-01", plan.daily).unwrap();
        let created = load(dir.path(), "task_1", "2026-11-01").unwrap().unwrap();
        assert_eq!(created.attempt, 1);
        assert!(created.next_phase.is_none());
        assert_eq!(
            created.evidence,
            vec!["wall-clock timeout after 900s".to_string()]
        );
        let text = inject_instruction("do the job", &created);
        assert!(text.contains("next_phase was not recorded"));
        assert!(text.contains("Do not redo completed work"));
    }

    #[test]
    fn evidence_keeps_the_newest_twenty_lines() {
        let dir = ws();
        let mut evidence = Vec::new();
        for i in 0..20 {
            evidence.push(format!("old-{i}"));
        }
        let seed = DailyRunProgress {
            task_id: "t".into(),
            date: "2026-11-01".into(),
            last_phase: None,
            next_phase: Some("P".into()),
            evidence,
            attempt: 4,
            updated_at: None,
        };
        write_progress(dir.path(), &seed).unwrap();
        apply(
            dir.path(),
            "t",
            "2026-11-01",
            DailyFileAction::Upsert {
                evidence: "newest".into(),
            },
        )
        .unwrap();
        let loaded = load(dir.path(), "t", "2026-11-01").unwrap().unwrap();
        assert_eq!(loaded.evidence.len(), 20);
        assert_eq!(loaded.evidence[0], "old-1");
        assert_eq!(loaded.evidence[19], "newest");
        assert_eq!(loaded.next_phase.as_deref(), Some("P"));
    }

    #[test]
    fn unsafe_task_id_does_not_escape_runs_dir() {
        let dir = ws();
        let err = apply(
            dir.path(),
            "../etc",
            "2026-11-01",
            DailyFileAction::Upsert {
                evidence: "x".into(),
            },
        );
        assert!(err.is_err());
        assert!(!dir.path().join("_runs").exists());
        assert!(apply(dir.path(), "ok", "2026/11/01", DailyFileAction::Delete,).is_err());
    }
}
