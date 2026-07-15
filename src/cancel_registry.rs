use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::oneshot;

/// A shared map of oneshot sender channels keyed by opaque ID.
/// Any tool can register a cancel channel; the Telegram callback handler
/// cancels by ID. Distinct from `CancellationToken` (used for /stop).
pub struct CancelRegistry {
    inner: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register(&self, id: String, tx: oneshot::Sender<()>) {
        let mut map = self.inner.blocking_lock();
        map.insert(id, tx);
    }

    pub fn cancel(&self, id: &str) -> bool {
        let mut map = self.inner.blocking_lock();
        if let Some(tx) = map.remove(id) {
            let _ = tx.send(());
            true
        } else {
            false
        }
    }

    pub fn unregister(&self, id: &str) {
        let mut map = self.inner.blocking_lock();
        map.remove(id);
    }
}

impl Default for CancelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_cancel() {
        let reg = CancelRegistry::new();
        let (tx, mut rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx);
        assert!(reg.cancel("cmd_1"));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn test_cancel_unknown() {
        let reg = CancelRegistry::new();
        assert!(!reg.cancel("nonexistent"));
    }

    #[test]
    fn test_unregister() {
        let reg = CancelRegistry::new();
        let (tx, _rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx);
        reg.unregister("cmd_1");
        assert!(!reg.cancel("cmd_1"));
    }

    #[test]
    fn test_double_cancel() {
        let reg = CancelRegistry::new();
        let (tx, _rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx);
        assert!(reg.cancel("cmd_1"));
        assert!(!reg.cancel("cmd_1"));
    }
}
