use std::collections::VecDeque;

use crate::llm::ToolCall;

/// A recorded tool call in the rolling window.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool_name: String,
    /// Hash of (tool_name + normalized JSON arguments).
    pub args_hash: u64,
    /// Iteration index when this call was made.
    pub iteration: usize,
}

/// Information returned when a loop is detected.
#[derive(Debug, Clone)]
pub struct LoopInfo {
    pub tool_name: String,
    pub call_count: usize,
}

/// Detects exact-repetition loops in tool call sequences.
///
/// Maintains a rolling FIFO window of recent tool calls. A loop is declared
/// when the last N entries all have the same (tool_name, args_hash).
pub struct LoopDetector {
    window: VecDeque<ToolCallRecord>,
    threshold: usize,
}

impl LoopDetector {
    pub fn new(threshold: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(threshold + 1),
            threshold,
        }
    }

    /// Normalize and hash tool call arguments for comparison.
    ///
    /// Sorts JSON keys alphabetically, trims whitespace, then computes a
    /// non-cryptographic hash of (tool_name + "|" + normalized_args).
    pub fn compute_hash(name: &str, arguments: &str) -> u64 {
        use std::hash::{Hash, Hasher};

        // Normalize: parse as JSON, sort keys, re-serialize.
        let normalized = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .map(normalize_json_value)
            .unwrap_or_else(|| arguments.trim().to_string());

        let mut hasher = rustc_hash::FxHasher::default();
        name.hash(&mut hasher);
        "|".hash(&mut hasher);
        normalized.hash(&mut hasher);
        hasher.finish()
    }

    /// Record a batch of tool calls from one iteration.
    pub fn record(&mut self, tool_calls: &[ToolCall], iteration: usize) {
        for tc in tool_calls {
            let hash = Self::compute_hash(&tc.function.name, &tc.function.arguments);
            self.window.push_back(ToolCallRecord {
                tool_name: tc.function.name.clone(),
                args_hash: hash,
                iteration,
            });
            while self.window.len() > self.threshold {
                self.window.pop_front();
            }
        }
    }

    /// Check whether a loop is currently detected.
    ///
    /// Returns `Some(LoopInfo)` when the last N entries all share the same
    /// (tool_name, args_hash), where N == threshold.
    pub fn detect_loop(&self) -> Option<LoopInfo> {
        if self.window.len() < self.threshold {
            return None;
        }

        let first = self.window.front()?;
        let all_same = self.window.iter().all(|r| r.args_hash == first.args_hash);

        if all_same {
            Some(LoopInfo {
                tool_name: first.tool_name.clone(),
                call_count: self.window.len(),
            })
        } else {
            None
        }
    }

    /// Clear the window — used after user approves continuation.
    pub fn clear(&mut self) {
        self.window.clear();
    }
}

/// Recursively sort all JSON object keys for deterministic comparison.
fn normalize_json_value(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, String)> = map
                .into_iter()
                .map(|(k, v)| (k, normalize_json_value(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let inner: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("\"{}\":{}", k, v))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.into_iter().map(normalize_json_value).collect();
            format!("[{}]", items.join(","))
        }
        serde_json::Value::String(s) => format!("\"{}\"", s.trim()),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn make_tool_call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "test_id".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    #[test]
    fn test_compute_hash_same_args_same_hash() {
        let a = LoopDetector::compute_hash("read_file", r#"{"path": "foo.txt"}"#);
        let b = LoopDetector::compute_hash("read_file", r#"{"path": "foo.txt"}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn test_compute_hash_different_args_different_hash() {
        let a = LoopDetector::compute_hash("read_file", r#"{"path": "a.txt"}"#);
        let b = LoopDetector::compute_hash("read_file", r#"{"path": "b.txt"}"#);
        assert_ne!(a, b);
    }

    #[test]
    fn test_compute_hash_key_order_invariance() {
        let a = LoopDetector::compute_hash("write_file", r#"{"content": "x", "path": "f.txt"}"#);
        let b = LoopDetector::compute_hash("write_file", r#"{"path": "f.txt", "content": "x"}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn test_compute_hash_whitespace_invariance() {
        let a = LoopDetector::compute_hash("read_file", r#"{"path":"x"}"#);
        let b = LoopDetector::compute_hash("read_file", r#"{"path": "x"}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn test_detect_below_threshold_returns_none() {
        let mut d = LoopDetector::new(3);
        d.record(&[make_tool_call("read", r#"{"path":"x"}"#)], 0);
        assert!(d.detect_loop().is_none());
    }

    #[test]
    fn test_detect_exact_threshold_detects() {
        let mut d = LoopDetector::new(3);
        let tc = make_tool_call("read", r#"{"path":"x"}"#);
        d.record(std::slice::from_ref(&tc), 0);
        d.record(std::slice::from_ref(&tc), 1);
        d.record(std::slice::from_ref(&tc), 2);
        let info = d.detect_loop().expect("loop should be detected");
        assert_eq!(info.tool_name, "read");
        assert_eq!(info.call_count, 3);
    }

    #[test]
    fn test_detect_three_different_returns_none() {
        let mut d = LoopDetector::new(3);
        d.record(&[make_tool_call("a", r#"{"path":"x"}"#)], 0);
        d.record(&[make_tool_call("b", r#"{"path":"x"}"#)], 1);
        d.record(&[make_tool_call("c", r#"{"path":"x"}"#)], 2);
        assert!(d.detect_loop().is_none());
    }

    #[test]
    fn test_clear_resets_detection() {
        let mut d = LoopDetector::new(3);
        let tc = make_tool_call("read", r#"{"path":"x"}"#);
        d.record(std::slice::from_ref(&tc), 0);
        d.record(std::slice::from_ref(&tc), 1);
        d.record(std::slice::from_ref(&tc), 2);
        assert!(d.detect_loop().is_some());
        d.clear();
        assert!(d.detect_loop().is_none());
    }

    #[test]
    fn test_detects_across_multiple_calls_per_iteration() {
        let mut d = LoopDetector::new(3);
        let tc = make_tool_call("read", r#"{"path":"x"}"#);
        // Two identical calls in iteration 0, one in iteration 1 = 3 total
        d.record(&[tc.clone(), tc.clone()], 0);
        d.record(std::slice::from_ref(&tc), 1);
        let info = d.detect_loop().expect("cross-turn loop detected");
        assert_eq!(info.tool_name, "read");
    }

    #[test]
    fn test_diff_tool_same_args_not_detected() {
        let mut d = LoopDetector::new(3);
        let tc_a = make_tool_call("read", r#"{"path":"x"}"#);
        let tc_b = make_tool_call("write", r#"{"path":"x"}"#);
        d.record(&[tc_a], 0);
        d.record(&[tc_b], 1);
        d.record(&[make_tool_call("read", r#"{"path":"x"}"#)], 2);
        assert!(d.detect_loop().is_none());
    }
}
