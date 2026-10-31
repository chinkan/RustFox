//! Scheduled-run completion (#330 / #333).
//!
//! A final reply is not proof the task happened. Interactive chat never calls
//! this module: guard rewrites stay user-visible there. The scheduler uses the
//! verdict below before it writes `scheduled_task_runs.status`.
//!
//! ## Verdict
//!
//! | `expected_artifact` | fresh file | guard rewrite | short clarify/menu | result |
//! | --- | --- | --- | --- | --- |
//! | unset | — | yes | — | `failed` + manual gate |
//! | unset | — | no | yes | `failed` + manual gate |
//! | unset | — | no | no | `completed` (compat, including legitimately short replies) |
//! | set | yes | — | — | `completed` (warn if a guard or the heuristic also fired) |
//! | set | no | — | — | `failed` + manual gate, reason names the glob |
//!
//! A fresh artifact is the completion proof when a glob is configured. The
//! guard flag and the short-reply heuristic are auxiliary: they do not
//! override a fresh file, and length under 200 alone never fails a run.
//!
//! ## Lookback
//!
//! A hit counts when `mtime >= run_started_at - lookback`. `lookback` is
//! `artifact_grace_secs` (default [`DEFAULT_ARTIFACT_GRACE_SECS`] = 900) and
//! is never tighter than [`ARTIFACT_PRESTART_SKEW_SECS`] (300). A file from
//! `run_started_at - 300s` therefore always counts. Set the column to 300 to
//! use that investigation window exactly. The default 900s also accepts a
//! file written in the few minutes before the run row was stamped.
//!
//! ## False positives
//!
//! The heuristic fails a short reply that quotes a clarify/menu phrase when
//! nothing fresh proves the work. A real short task whose text happens to
//! contain one of those phrases (for example "未收到" inside a weather note)
//! is failed until an `expected_artifact` glob matches. Do not use the
//! heuristic as the only gate for tasks that are supposed to be short.

use crate::loop_runner::GuardKind;
use chrono::{DateTime, Duration, Utc};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Default `scheduled_tasks.artifact_grace_secs`.
pub const DEFAULT_ARTIFACT_GRACE_SECS: i64 = 900;

/// Minimum lookback. #333's `mtime >= run_started_at - 300s` always counts.
pub const ARTIFACT_PRESTART_SKEW_SECS: i64 = 300;

/// What the filesystem check found for `expected_artifact`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactProof {
    /// Glob unset or blank. Compat path.
    Unconfigured,
    /// At least one regular file matched and was new enough.
    Fresh,
    /// Glob was searched; nothing fresh matched.
    Missing,
    /// Pattern or filesystem error. Fail closed.
    ScanFailed { detail: String },
}

/// Inputs the scheduler already has when it is about to write `completed`.
pub struct ScheduleCompletionInput<'a> {
    pub response: &'a str,
    pub rewritten_by_guard: Option<GuardKind>,
    /// Raw column. Blank is treated as unset.
    pub expected_artifact: Option<&'a str>,
    pub artifact: ArtifactProof,
}

/// What to persist, and whether the manual dead-letter gate runs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum ScheduleCompletion {
    /// `status = completed`. `warning` is logged, not stored as an error.
    Completed { warning: Option<String> },
    /// `status = failed`. Caller enqueues the manual gate (`enqueue_manual`
    /// or `mark_awaiting_user`), not the auto re-fire queue.
    Failed { fail_reason: String },
}

impl ScheduleCompletion {
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    pub fn run_status(&self) -> &'static str {
        if self.is_completed() {
            "completed"
        } else {
            "failed"
        }
    }

    /// Human gate once. Never the transient auto-`enqueue` path: a no-op may
    /// already have partial side effects.
    pub fn manual_gate(&self) -> bool {
        !self.is_completed()
    }
}

/// Lookback applied to `run_started_at`. `<= 0` means the default 900s.
/// Values between 1 and 299 are raised to 300s.
pub fn effective_lookback_secs(grace_secs: i64) -> i64 {
    let grace = if grace_secs <= 0 {
        DEFAULT_ARTIFACT_GRACE_SECS
    } else {
        grace_secs
    };
    grace.max(ARTIFACT_PRESTART_SKEW_SECS)
}

pub fn artifact_not_before(run_started_at: DateTime<Utc>, grace_secs: i64) -> DateTime<Utc> {
    run_started_at - Duration::seconds(effective_lookback_secs(grace_secs))
}

/// Write-path check. The scanner floors `1..=299` up to 300s and treats
/// `<= 0` as the default 900s, but a stored row must not claim a tighter
/// window than [`ARTIFACT_PRESTART_SKEW_SECS`].
pub fn validate_artifact_grace(secs: i64) -> Result<(), String> {
    if secs < ARTIFACT_PRESTART_SKEW_SECS {
        Err(format!(
            "artifact_grace_secs must be >= {ARTIFACT_PRESTART_SKEW_SECS}"
        ))
    } else {
        Ok(())
    }
}

/// Reject globs the scanner will not run. Relative patterns are resolved
/// under the RustFox home later; this only checks the pattern text.
pub fn validate_artifact_glob(pattern: &str) -> Result<(), String> {
    let parts = split_glob(pattern)?;
    if Path::new(pattern.trim()).is_absolute() && literal_prefix_len(&parts) == 0 {
        return Err("absolute artifact glob must include a literal directory prefix".to_string());
    }
    Ok(())
}

/// Search `expected_artifact`. `home` is the root for relative patterns.
pub fn prove_artifact(
    home: Option<&Path>,
    expected_artifact: Option<&str>,
    run_started_at: DateTime<Utc>,
    grace_secs: i64,
) -> ArtifactProof {
    let Some(glob) = expected_artifact.map(str::trim).filter(|s| !s.is_empty()) else {
        return ArtifactProof::Unconfigured;
    };
    match scan_expected_artifact(home, glob, run_started_at, grace_secs) {
        Ok(true) => ArtifactProof::Fresh,
        Ok(false) => ArtifactProof::Missing,
        Err(detail) => ArtifactProof::ScanFailed { detail },
    }
}

pub fn judge_schedule_completion(input: &ScheduleCompletionInput<'_>) -> ScheduleCompletion {
    if let Some(glob) = configured_glob(input.expected_artifact) {
        if matches!(input.artifact, ArtifactProof::Fresh) {
            return ScheduleCompletion::Completed {
                warning: auxiliary_warning(input),
            };
        }
        let mut reason = match &input.artifact {
            ArtifactProof::ScanFailed { detail } => {
                format!("expected artifact unreadable: {glob}: {detail}")
            }
            _ => format!("expected artifact missing: {glob}"),
        };
        if let Some(kind) = input.rewritten_by_guard {
            reason.push_str(&format!("; rewritten_by_guard={}", kind.as_str()));
        } else if looks_like_noop_reply(input.response) {
            reason.push_str("; short clarify/menu reply");
        }
        return ScheduleCompletion::Failed {
            fail_reason: reason,
        };
    }

    // No glob: #330's guard flag is decisive. The heuristic covers model
    // no-ops that never touched a guard (#333), and only when the phrasing
    // matches — a short substantive reply stays completed.
    if let Some(kind) = input.rewritten_by_guard {
        return ScheduleCompletion::Failed {
            fail_reason: format!("guard rewrite ({}) is not task completion", kind.as_str()),
        };
    }
    if looks_like_noop_reply(input.response) {
        return ScheduleCompletion::Failed {
            fail_reason: "short clarify/menu reply with no artifact proof".to_string(),
        };
    }
    ScheduleCompletion::Completed { warning: None }
}

fn configured_glob(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|s| !s.is_empty())
}

fn auxiliary_warning(input: &ScheduleCompletionInput<'_>) -> Option<String> {
    if let Some(kind) = input.rewritten_by_guard {
        return Some(format!(
            "guard rewrite ({}) ignored because a fresh artifact matched",
            kind.as_str()
        ));
    }
    if looks_like_noop_reply(input.response) {
        return Some(
            "short clarify/menu reply ignored because a fresh artifact matched".to_string(),
        );
    }
    None
}

/// Short reply that asks the user to restate the task, or a capability menu.
///
/// Length under 200 is required and is not sufficient. See the module docs
/// for the false-positive risk.
fn looks_like_noop_reply(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() >= 200 {
        return false;
    }
    let lower = trimmed.to_lowercase();
    const PHRASES: &[&str] = &[
        "你再 send 一次",
        "你想我做咩",
        "想繼續之前邊件事",
        "即係做咗未",
        "未收到",
        "冇收到",
        "Let's continue from the prior thread",
    ];
    if PHRASES.iter().any(|p| trimmed.contains(p)) {
        return true;
    }
    const ASCII: &[&str] = &[
        "didn't receive",
        "without progress",
        "wasn't making progress",
    ];
    if ASCII.iter().any(|p| lower.contains(p)) {
        return true;
    }
    let dashes = trimmed
        .lines()
        .filter(|line| line.trim_start().starts_with("- "))
        .count();
    if dashes >= 3 {
        let fox = trimmed.contains("Fox 在線") || trimmed.contains("Fox 喺度");
        if fox || lower.contains("skill") {
            return true;
        }
    }
    false
}

fn scan_expected_artifact(
    home: Option<&Path>,
    pattern: &str,
    run_started_at: DateTime<Utc>,
    grace_secs: i64,
) -> Result<bool, String> {
    validate_artifact_glob(pattern)?;
    let parts = split_glob(pattern)?;
    let nlit = literal_prefix_len(&parts);
    let (literals, rest) = parts.split_at(nlit);
    let start = if Path::new(pattern.trim()).is_absolute() {
        let mut path = PathBuf::from("/");
        for lit in literals {
            path.push(lit);
        }
        path
    } else {
        let home = home.ok_or_else(|| {
            "RustFox home is unset; relative artifact glob cannot be resolved".to_string()
        })?;
        let mut path = home.to_path_buf();
        for lit in literals {
            path.push(lit);
        }
        path
    };
    let not_before = utc_to_system(artifact_not_before(run_started_at, grace_secs));
    if rest.is_empty() {
        return file_is_fresh(&start, not_before);
    }
    walk(&start, rest, 0, not_before)
}

fn split_glob(pattern: &str) -> Result<Vec<String>, String> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return Err("artifact glob is empty".to_string());
    }
    if trimmed.contains('\0') {
        return Err("artifact glob contains NUL".to_string());
    }
    let parts: Vec<String> = trimmed
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if parts.is_empty() {
        return Err("artifact glob is empty".to_string());
    }
    if parts.iter().any(|p| p == "..") {
        return Err("artifact glob must not contain ..".to_string());
    }
    Ok(parts)
}

fn literal_prefix_len(parts: &[String]) -> usize {
    parts
        .iter()
        .position(|p| p.contains('*') || p.contains('?'))
        .unwrap_or(parts.len())
}

fn utc_to_system(dt: DateTime<Utc>) -> SystemTime {
    match dt.timestamp_nanos_opt() {
        Some(nanos) if nanos > 0 => {
            SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(nanos as u64)
        }
        _ => SystemTime::UNIX_EPOCH,
    }
}

fn file_is_fresh(path: &Path, not_before: SystemTime) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => {
            let modified = meta
                .modified()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(modified >= not_before)
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn walk(dir: &Path, parts: &[String], depth: u32, not_before: SystemTime) -> Result<bool, String> {
    if parts.is_empty() || depth > 128 {
        return Ok(false);
    }
    let head = &parts[0];
    let rest = &parts[1..];
    if head == "**" {
        if !rest.is_empty() && walk(dir, rest, depth + 1, not_before)? {
            return Ok(true);
        }
        return walk_children(dir, true, rest, depth, not_before);
    }
    walk_children(dir, false, parts, depth, not_before)
}

fn walk_children(
    dir: &Path,
    globstar: bool,
    parts_or_rest: &[String],
    depth: u32,
    not_before: SystemTime,
) -> Result<bool, String> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for ent in rd {
        let ent = ent.map_err(|e| format!("{}: {e}", dir.display()))?;
        let ft = ent
            .file_type()
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        if ft.is_symlink() {
            continue;
        }
        if globstar {
            if ft.is_dir() {
                let mut again = Vec::with_capacity(parts_or_rest.len() + 1);
                again.push("**".to_string());
                again.extend(parts_or_rest.iter().cloned());
                if walk(&ent.path(), &again, depth + 1, not_before)? {
                    return Ok(true);
                }
            }
            if parts_or_rest.is_empty() && ft.is_file() && entry_fresh(&ent, not_before)? {
                return Ok(true);
            }
            continue;
        }
        let name = ent.file_name();
        let name = name.to_string_lossy();
        let head = &parts_or_rest[0];
        let rest = &parts_or_rest[1..];
        if !segment_match(head, &name) {
            continue;
        }
        if rest.is_empty() {
            if ft.is_file() && entry_fresh(&ent, not_before)? {
                return Ok(true);
            }
        } else if ft.is_dir() && walk(&ent.path(), rest, depth + 1, not_before)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn entry_fresh(ent: &std::fs::DirEntry, not_before: SystemTime) -> Result<bool, String> {
    let modified = ent
        .metadata()
        .and_then(|m| m.modified())
        .map_err(|e| format!("{}: {e}", ent.path().display()))?;
    Ok(modified >= not_before)
}

fn segment_match(pat: &str, name: &str) -> bool {
    fn rec(pat: &[char], name: &[char]) -> bool {
        if pat.is_empty() {
            return name.is_empty();
        }
        if pat[0] == '*' {
            if rec(&pat[1..], name) {
                return true;
            }
            return !name.is_empty() && rec(pat, &name[1..]);
        }
        if name.is_empty() {
            return false;
        }
        if pat[0] == '?' || pat[0] == name[0] {
            return rec(&pat[1..], &name[1..]);
        }
        false
    }
    let pat: Vec<char> = pat.chars().collect();
    let name: Vec<char> = name.chars().collect();
    rec(&pat, &name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cold_intro::COLD_INTRO_CLARIFY_REWRITE;
    use crate::loop_runner::EMPTY_PROGRESS_CLARIFY;
    use chrono::TimeZone;
    use std::fs;

    fn judge(
        response: &str,
        guard: Option<GuardKind>,
        glob: Option<&str>,
        artifact: ArtifactProof,
    ) -> ScheduleCompletion {
        judge_schedule_completion(&ScheduleCompletionInput {
            response,
            rewritten_by_guard: guard,
            expected_artifact: glob,
            artifact,
        })
    }

    fn assert_failed(verdict: &ScheduleCompletion) -> String {
        assert!(
            !verdict.is_completed(),
            "must not be completed: {verdict:?}"
        );
        assert_eq!(verdict.run_status(), "failed");
        assert!(verdict.manual_gate());
        match verdict {
            ScheduleCompletion::Failed { fail_reason } => fail_reason.clone(),
            ScheduleCompletion::Completed { .. } => unreachable!(),
        }
    }

    fn started() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap()
    }

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("rustfox-art-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_at(path: &Path, when: DateTime<Utc>) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let file = fs::File::create(path).unwrap();
        file.set_modified(utc_to_system(when)).unwrap();
    }

    /// #330: the guard's own clarify sentence as a schedule FinalResponse
    /// must not be stored as completed, and must take the manual gate.
    #[test]
    fn cold_intro_clarify_rewrite_final_response_on_schedule_path_is_not_completed() {
        let verdict = judge(
            COLD_INTRO_CLARIFY_REWRITE,
            Some(GuardKind::ColdIntroClarify),
            None,
            ArtifactProof::Unconfigured,
        );
        let reason = assert_failed(&verdict);
        assert!(
            reason.contains("cold_intro_clarify"),
            "reason should identify the guard, got {reason}"
        );
    }

    #[test]
    fn guard_flag_fails_a_long_reply_when_no_artifact_is_configured() {
        let long = "x".repeat(500);
        let reason = assert_failed(&judge(
            &long,
            Some(GuardKind::LostContextClarify),
            None,
            ArtifactProof::Unconfigured,
        ));
        assert!(reason.contains("lost_context_clarify"), "{reason}");
    }

    #[test]
    fn fresh_artifact_keeps_guard_rewrite_completed() {
        let verdict = judge(
            COLD_INTRO_CLARIFY_REWRITE,
            Some(GuardKind::ColdIntroClarify),
            Some("workspace/newsheet/frontpage-*.jpg"),
            ArtifactProof::Fresh,
        );
        assert!(verdict.is_completed());
        assert!(!verdict.manual_gate());
        let ScheduleCompletion::Completed { warning: Some(w) } = verdict else {
            panic!("expected a warning, got {verdict:?}");
        };
        assert!(w.contains("cold_intro_clarify"), "{w}");
    }

    #[test]
    fn null_artifact_long_reply_stays_completed() {
        let verdict = judge(&"x".repeat(400), None, None, ArtifactProof::Unconfigured);
        assert!(verdict.is_completed());
        assert!(!verdict.manual_gate());
    }

    #[test]
    fn short_substantive_reply_is_not_failed_on_length_alone() {
        let verdict = judge(
            "晴，28°C，濕度 75%。",
            None,
            None,
            ArtifactProof::Unconfigured,
        );
        assert!(
            verdict.is_completed(),
            "legitimately short tasks must stay completed"
        );
    }

    #[test]
    fn short_clarify_phrase_fails_when_no_artifact() {
        let reason = assert_failed(&judge(
            "上一句我個邊收唔到內容。你再 send 一次？",
            None,
            None,
            ArtifactProof::Unconfigured,
        ));
        assert!(reason.contains("no artifact proof"), "{reason}");
    }

    #[test]
    fn clarify_phrase_at_200_chars_is_not_the_heuristic() {
        let mut text = "你想我做咩".to_string();
        while text.chars().count() < 200 {
            text.push('x');
        }
        assert_eq!(text.chars().count(), 200);
        assert!(judge(&text, None, None, ArtifactProof::Unconfigured).is_completed());

        let mut under = "你想我做咩".to_string();
        while under.chars().count() < 199 {
            under.push('x');
        }
        assert_eq!(under.chars().count(), 199);
        assert!(!judge(&under, None, None, ArtifactProof::Unconfigured).is_completed());
    }

    #[test]
    fn short_menu_without_fox_or_skill_stays_completed() {
        let list = "Bought:\n- eggs\n- milk\n- bread";
        assert!(list.chars().count() < 200);
        assert!(judge(list, None, None, ArtifactProof::Unconfigured).is_completed());
    }

    #[test]
    fn short_fox_menu_fails_when_no_artifact() {
        let menu = "Fox 在線\n- 財報\n- vault\n- threads 帖";
        assert!(menu.chars().count() < 200);
        assert!(!judge(menu, None, None, ArtifactProof::Unconfigured).is_completed());
    }

    #[test]
    fn empty_progress_clarify_without_flag_fails_when_no_artifact() {
        assert!(!judge(
            EMPTY_PROGRESS_CLARIFY,
            None,
            None,
            ArtifactProof::Unconfigured
        )
        .is_completed());
    }

    #[test]
    fn missing_glob_fails_even_for_a_long_report() {
        let reason = assert_failed(&judge(
            &"x".repeat(400),
            None,
            Some("workspace/newsheet/frontpage-*.jpg"),
            ArtifactProof::Missing,
        ));
        assert!(
            reason.contains("expected artifact missing: workspace/newsheet/frontpage-*.jpg"),
            "{reason}"
        );
    }

    #[test]
    fn fresh_artifact_overrides_short_clarify_heuristic() {
        let verdict = judge(
            "你想我做咩",
            None,
            Some("workspace/newsheet/frontpage-*.jpg"),
            ArtifactProof::Fresh,
        );
        assert!(verdict.is_completed());
    }

    #[test]
    fn blank_glob_is_the_compat_path() {
        assert!(judge("晴，28°C。", None, Some("   "), ArtifactProof::Missing).is_completed());
    }

    #[test]
    fn lookback_defaults_to_900_and_floors_at_300() {
        let start = started();
        assert_eq!(
            artifact_not_before(start, DEFAULT_ARTIFACT_GRACE_SECS),
            start - Duration::seconds(900)
        );
        assert_eq!(
            artifact_not_before(start, 0),
            start - Duration::seconds(900)
        );
        assert_eq!(
            artifact_not_before(start, 100),
            start - Duration::seconds(300)
        );
        assert_eq!(
            artifact_not_before(start, 300),
            start - Duration::seconds(300)
        );
        assert_eq!(
            artifact_not_before(start, 3600),
            start - Duration::seconds(3600)
        );
    }

    #[test]
    fn glob_match_requires_mtime_within_lookback() {
        let root = TmpDir::new();
        let fresh = root.0.join("workspace/newsheet/frontpage-2026-10-30.jpg");
        let stale = root.0.join("workspace/newsheet/frontpage-old.jpg");
        let start = started();
        // Exactly the #333 skew: 300s before the run still counts under the
        // default 900s grace, and under an explicit 300s grace.
        write_at(&fresh, start - Duration::seconds(300));
        write_at(&stale, start - Duration::seconds(901));
        let glob = "workspace/newsheet/frontpage-*.jpg";
        assert!(
            scan_expected_artifact(Some(&root.0), glob, start, DEFAULT_ARTIFACT_GRACE_SECS)
                .unwrap(),
            "mtime >= run_started_at-300s must match"
        );
        assert!(
            scan_expected_artifact(Some(&root.0), glob, start, 300).unwrap(),
            "explicit 300s grace still accepts the 300s-old file"
        );

        let only_stale = TmpDir::new();
        let old = only_stale.0.join("workspace/newsheet/frontpage-old.jpg");
        write_at(&old, start - Duration::seconds(901));
        assert!(!scan_expected_artifact(
            Some(&only_stale.0),
            glob,
            start,
            DEFAULT_ARTIFACT_GRACE_SECS
        )
        .unwrap());
        write_at(&old, start - Duration::seconds(301));
        assert!(
            !scan_expected_artifact(Some(&only_stale.0), glob, start, 300).unwrap(),
            "301s before start is outside a 300s grace"
        );
    }

    #[test]
    fn glob_matches_nested_and_rejects_symlink_and_dotdot() {
        let root = TmpDir::new();
        let start = started();
        let note = root.0.join("vault/daily/nested/note-1.md");
        write_at(&note, start);
        assert!(scan_expected_artifact(
            Some(&root.0),
            "vault/**/note-*.md",
            start,
            DEFAULT_ARTIFACT_GRACE_SECS
        )
        .unwrap());

        let real = root.0.join("real.txt");
        write_at(&real, start);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, root.0.join("link.txt")).unwrap();
            assert!(!scan_expected_artifact(
                Some(&root.0),
                "link.txt",
                start,
                DEFAULT_ARTIFACT_GRACE_SECS
            )
            .unwrap());
        }
        assert!(scan_expected_artifact(
            Some(&root.0),
            "real.txt",
            start,
            DEFAULT_ARTIFACT_GRACE_SECS
        )
        .unwrap());

        fs::create_dir(root.0.join("not-a-file.md")).unwrap();
        assert!(!scan_expected_artifact(
            Some(&root.0),
            "not-a-file.md",
            start,
            DEFAULT_ARTIFACT_GRACE_SECS
        )
        .unwrap());

        let err = scan_expected_artifact(
            Some(&root.0),
            "../secret/*.md",
            start,
            DEFAULT_ARTIFACT_GRACE_SECS,
        )
        .unwrap_err();
        assert!(err.contains(".."), "{err}");
        assert!(validate_artifact_glob("/*.jpg").is_err());
        assert!(validate_artifact_glob("/tmp/out/*.jpg").is_ok());
    }

    #[test]
    fn missing_directory_is_not_a_scan_error() {
        let root = TmpDir::new();
        assert!(!scan_expected_artifact(
            Some(&root.0),
            "workspace/newsheet/frontpage-*.jpg",
            started(),
            DEFAULT_ARTIFACT_GRACE_SECS
        )
        .unwrap());
    }
}
