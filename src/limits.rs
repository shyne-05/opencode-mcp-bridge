use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const QUEUE_WAIT: Duration = Duration::from_secs(5);

/// Bound running work and callers waiting for it, with automatic release on cancellation.
#[derive(Clone)]
pub struct ToolGate {
    running: Arc<Semaphore>,
    admitted: Arc<Semaphore>,
}

#[derive(Debug)]
pub struct ToolPermit {
    _running: OwnedSemaphorePermit,
    _admitted: OwnedSemaphorePermit,
}

impl ToolGate {
    pub fn new(concurrency: usize) -> Self {
        Self {
            running: Arc::new(Semaphore::new(concurrency)),
            admitted: Arc::new(Semaphore::new(concurrency.saturating_mul(4))),
        }
    }

    pub async fn acquire(&self, tool: &str) -> Result<ToolPermit, String> {
        self.acquire_with_timeout(tool, QUEUE_WAIT).await
    }

    async fn acquire_with_timeout(
        &self,
        tool: &str,
        timeout: Duration,
    ) -> Result<ToolPermit, String> {
        let admitted = self
            .admitted
            .clone()
            .try_acquire_owned()
            .map_err(|_| format!("{tool} is busy; please try again shortly"))?;
        let running = tokio::time::timeout(timeout, self.running.clone().acquire_owned())
            .await
            .map_err(|_| format!("{tool} is busy; waiting for an available slot timed out"))?
            .map_err(|_| format!("{tool} is shutting down"))?;
        Ok(ToolPermit {
            _running: running,
            _admitted: admitted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queue_deadline_releases_admission_for_later_work() {
        let gate = ToolGate::new(1);
        let active = gate.acquire("test").await.unwrap();
        let error = gate
            .acquire_with_timeout("test", Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(error.contains("timed out"));
        assert_eq!(gate.admitted.available_permits(), 3);
        drop(active);
        assert!(gate.acquire("test").await.is_ok());
    }

    #[tokio::test]
    async fn queue_capacity_is_bounded_and_cancellation_frees_it() {
        let gate = ToolGate::new(1);
        let active = gate.acquire("test").await.unwrap();
        let mut waiting = Vec::new();
        for _ in 0..3 {
            let queued_gate = gate.clone();
            waiting.push(tokio::spawn(
                async move { queued_gate.acquire("test").await },
            ));
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while gate.admitted.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(gate.acquire("test").await.unwrap_err().contains("busy"));
        for task in waiting {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        assert_eq!(gate.admitted.available_permits(), 3);
        drop(active);
        assert_eq!(gate.running.available_permits(), 1);
        assert_eq!(gate.admitted.available_permits(), 4);
    }

    #[tokio::test]
    async fn cloned_gates_share_the_running_limit() {
        let gate = ToolGate::new(2);
        let clone = gate.clone();
        let first = gate.acquire("test").await.unwrap();
        let second = clone.acquire("test").await.unwrap();
        assert_eq!(gate.running.available_permits(), 0);
        drop(first);
        assert!(clone.acquire("test").await.is_ok());
        drop(second);
        assert_eq!(gate.running.available_permits(), 2);
    }
}
