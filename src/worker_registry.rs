// In-memory map of session_id -> WorkerHandle. Owns the stream-json claude
// children spawned by orc. Replaces tmux as the worker process layer.

use crate::worker::WorkerHandle;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct WorkerRegistry {
    inner: Arc<Mutex<HashMap<String, WorkerHandle>>>,
}

impl WorkerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, handle: WorkerHandle) {
        let id = handle.session_id.clone();
        self.inner.lock().await.insert(id, handle);
    }

    pub async fn send(&self, session_id: &str, msg: &str) -> Result<()> {
        let map = self.inner.lock().await;
        if let Some(h) = map.get(session_id) {
            h.send(msg).await?;
        }
        Ok(())
    }

    pub async fn interrupt(&self, session_id: &str) -> Result<()> {
        let map = self.inner.lock().await;
        if let Some(h) = map.get(session_id) {
            h.interrupt().await?;
        }
        Ok(())
    }

    pub async fn kill(&self, session_id: &str) -> Result<()> {
        let mut map = self.inner.lock().await;
        if let Some(h) = map.remove(session_id) {
            h.kill().await?;
        }
        Ok(())
    }

    pub async fn contains(&self, session_id: &str) -> bool {
        self.inner.lock().await.contains_key(session_id)
    }

    pub async fn ids(&self) -> Vec<String> {
        self.inner.lock().await.keys().cloned().collect()
    }

    pub async fn kill_all(&self) {
        let mut map = self.inner.lock().await;
        for (_, h) in map.drain() {
            let _ = h.kill().await;
        }
    }
}
