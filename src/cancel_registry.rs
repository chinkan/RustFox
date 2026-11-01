use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::sync::Mutex;

tokio::task_local! {
    /// Lane scope stamped onto tool-cancel registrations (`execute_command`).
    ///
    /// Set for the duration of one agent loop. Empty when unset. A schedule
    /// timeout cancels only its scope so an interactive turn's tools keep
    /// running (ADR-0052 / #338).
    pub static CANCEL_SCOPE: String;
}

struct Entry {
    scope: String,
    tx: oneshot::Sender<()>,
}

/// A shared map of oneshot sender channels keyed by opaque ID.
/// Any tool can register a cancel channel; the Telegram callback handler
/// cancels by ID. Distinct from `CancellationToken` (used for /stop).
pub struct CancelRegistry {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn register(&self, id: String, tx: oneshot::Sender<()>) {
        let scope = CANCEL_SCOPE.try_with(|s| s.clone()).unwrap_or_default();
        let mut map = self.inner.lock().await;
        map.insert(id, Entry { scope, tx });
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let mut map = self.inner.lock().await;
        if let Some(entry) = map.remove(id) {
            let _ = entry.tx.send(());
            true
        } else {
            false
        }
    }

    /// Cancel every in-flight tool registered under `scope`.
    ///
    /// An empty scope cancels nothing, so unscoped registrations (tests, turns
    /// that never set [`CANCEL_SCOPE`]) are not wiped by a blank key.
    pub async fn cancel_scope(&self, scope: &str) -> usize {
        if scope.is_empty() {
            return 0;
        }
        let mut map = self.inner.lock().await;
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, entry)| entry.scope == scope)
            .map(|(id, _)| id.clone())
            .collect();
        let n = ids.len();
        for id in ids {
            if let Some(entry) = map.remove(&id) {
                let _ = entry.tx.send(());
            }
        }
        n
    }

    /// Cancel every registered in-flight tool (e.g. execute_command).
    /// Used on `/stop`, which cancels both lanes (ADR-0024).
    pub async fn cancel_all(&self) -> usize {
        let mut map = self.inner.lock().await;
        let n = map.len();
        for (_id, entry) in map.drain() {
            let _ = entry.tx.send(());
        }
        n
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

    #[tokio::test]
    async fn test_cancel_all_fires_every_channel() {
        let reg = CancelRegistry::new();
        let (tx1, mut rx1) = oneshot::channel();
        let (tx2, mut rx2) = oneshot::channel();
        reg.register("a".into(), tx1).await;
        reg.register("b".into(), tx2).await;
        assert_eq!(reg.cancel_all().await, 2);
        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
        assert_eq!(reg.cancel_all().await, 0);
    }

    #[tokio::test]
    async fn cancel_scope_does_not_cancel_other_lane() {
        let reg = CancelRegistry::new();
        let (tx_sched, mut rx_sched) = oneshot::channel();
        let (tx_user, mut rx_user) = oneshot::channel();
        CANCEL_SCOPE
            .scope("main:1:sched".to_string(), async {
                reg.register("sched-cmd".into(), tx_sched).await;
            })
            .await;
        CANCEL_SCOPE
            .scope("main:1".to_string(), async {
                reg.register("user-cmd".into(), tx_user).await;
            })
            .await;
        assert_eq!(reg.cancel_scope("main:1:sched").await, 1);
        assert!(rx_sched.try_recv().is_ok());
        assert!(rx_user.try_recv().is_err());
        assert!(reg.cancel("user-cmd").await);
        assert!(rx_user.try_recv().is_ok());
        assert_eq!(reg.cancel_scope("").await, 0);
    }
}
