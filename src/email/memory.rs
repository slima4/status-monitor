use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::trait_def::{EmailResult, EmailSender, MessageId, TransactionalEmail};

#[derive(Default, Clone)]
pub struct InMemoryEmailSender {
    inner: Arc<Mutex<Vec<TransactionalEmail>>>,
}

impl InMemoryEmailSender {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of every email send() has accepted. Cheap clone of the inner
    /// vector; tests typically read once at assertion time.
    pub fn sent(&self) -> Vec<TransactionalEmail> {
        self.inner
            .lock()
            .expect("memory sender mutex poisoned")
            .clone()
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("memory sender mutex poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.inner
            .lock()
            .expect("memory sender mutex poisoned")
            .clear();
    }
}

#[async_trait]
impl EmailSender for InMemoryEmailSender {
    async fn send(&self, email: TransactionalEmail) -> EmailResult<MessageId> {
        let mut guard = self.inner.lock().expect("memory sender mutex poisoned");
        let id = MessageId(format!("memory-{}", guard.len()));
        guard.push(email);
        Ok(id)
    }
}
