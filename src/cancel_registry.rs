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

    pub async fn register(&self, id: String, tx: oneshot::Sender<()>) {
        let mut map = self.inner.lock().await;
        map.insert(id, tx);
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let mut map = self.inner.lock().await;
        if let Some(tx) = map.remove(id) {
            let _ = tx.send(());
            true
        } else {
            false
        }
    }

    pub async fn unregister(&self, id: &str) {
        let mut map = self.inner.lock().await;
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

    #[tokio::test]
    async fn test_register_and_cancel() {
        let reg = CancelRegistry::new();
        let (tx, mut rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx).await;
        assert!(reg.cancel("cmd_1").await);
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn test_cancel_unknown() {
        let reg = CancelRegistry::new();
        assert!(!reg.cancel("nonexistent").await);
    }

    #[tokio::test]
    async fn test_unregister() {
        let reg = CancelRegistry::new();
        let (tx, _rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx).await;
        reg.unregister("cmd_1").await;
        assert!(!reg.cancel("cmd_1").await);
    }

    #[tokio::test]
    async fn test_double_cancel() {
        let reg = CancelRegistry::new();
        let (tx, _rx) = oneshot::channel();
        reg.register("cmd_1".to_string(), tx).await;
        assert!(reg.cancel("cmd_1").await);
        assert!(!reg.cancel("cmd_1").await);
    }
}
