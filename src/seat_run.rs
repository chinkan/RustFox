//! Lightweight in-process seat←run signal (ADR-0050 Decision 4 / #309).
//!
//! Refcounted set of in-flight agent names. Portal `GET /api/agents` marks a
//! seat `running` when the name is present. Not a full activity bus / SSE.
//! Cleared on Drop of [`SeatRunGuard`] (completion, cancel, or early return).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Process-wide (per Agent) refcounted map of in-flight seat names.
#[derive(Clone, Default)]
pub struct SeatRunSignal {
    counts: Arc<Mutex<HashMap<String, usize>>>,
}

impl SeatRunSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the refcount for `name`. Empty / whitespace names are ignored
    /// (no seat to light). Drop the returned guard to decrement.
    pub fn enter(&self, name: &str) -> SeatRunGuard {
        let name = name.trim();
        if name.is_empty() {
            return SeatRunGuard {
                counts: Arc::clone(&self.counts),
                name: None,
            };
        }
        {
            let mut map = self.counts.lock().unwrap_or_else(|e| e.into_inner());
            *map.entry(name.to_string()).or_insert(0) += 1;
        }
        SeatRunGuard {
            counts: Arc::clone(&self.counts),
            name: Some(name.to_string()),
        }
    }

    /// True when at least one in-flight invoke/spawn is tagged to `name`.
    pub fn is_running(&self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        self.counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .copied()
            .unwrap_or(0)
            > 0
    }
}

/// RAII decrement for one [`SeatRunSignal::enter`] call.
pub struct SeatRunGuard {
    counts: Arc<Mutex<HashMap<String, usize>>>,
    name: Option<String>,
}

impl Drop for SeatRunGuard {
    fn drop(&mut self) {
        let Some(name) = self.name.take() else {
            return;
        };
        let mut map = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = map.get_mut(&name) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&name);
            }
        }
    }
}

/// Honest seat status for ADR-0050 v1 (`idle` | `running`; `error` reserved).
///
/// Never invents busy: when `live_signal` is false the seat stays `idle` even
/// if a stale flag claimed otherwise. `definition_only` is the inverse of
/// `live_signal` so clients never fake busy for definition-only rows.
pub fn seat_status(live_signal: bool, flagged_running: bool) -> (&'static str, bool) {
    let definition_only = !live_signal;
    let status = if live_signal && flagged_running {
        "running"
    } else {
        "idle"
    };
    (status, definition_only)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_marks_running_leave_clears() {
        let signal = SeatRunSignal::new();
        assert!(!signal.is_running("researcher"));
        {
            let _g = signal.enter("researcher");
            assert!(signal.is_running("researcher"));
            assert!(!signal.is_running("verifier"));
        }
        assert!(!signal.is_running("researcher"));
    }

    #[test]
    fn refcount_survives_overlapping_enters() {
        let signal = SeatRunSignal::new();
        let g1 = signal.enter("scribe");
        let g2 = signal.enter("scribe");
        assert!(signal.is_running("scribe"));
        drop(g1);
        assert!(signal.is_running("scribe"));
        drop(g2);
        assert!(!signal.is_running("scribe"));
    }

    #[test]
    fn empty_name_is_noop() {
        let signal = SeatRunSignal::new();
        let _g = signal.enter("   ");
        assert!(!signal.is_running(""));
        assert!(!signal.is_running("   "));
    }

    #[test]
    fn seat_status_never_fakes_busy_without_live_signal() {
        let (status, definition_only) = seat_status(false, true);
        assert_eq!(status, "idle");
        assert!(definition_only);

        let (status, definition_only) = seat_status(false, false);
        assert_eq!(status, "idle");
        assert!(definition_only);
    }

    #[test]
    fn seat_status_running_only_with_live_signal_and_flag() {
        let (status, definition_only) = seat_status(true, true);
        assert_eq!(status, "running");
        assert!(!definition_only);

        let (status, definition_only) = seat_status(true, false);
        assert_eq!(status, "idle");
        assert!(!definition_only);
    }
}
