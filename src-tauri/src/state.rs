use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Message {
    pub id: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ts: f64,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub mime: String,
}

#[derive(Debug, Default)]
pub struct Inner {
    pub messages: Vec<Message>,
    pub files: HashMap<String, FileEntry>,
    pub pending_saves: HashMap<String, String>,
    pub connected: bool,
}

pub struct SharedStateInner {
    pub data: Mutex<Inner>,
    /// Каждый push инкрементирует значение — poll-ы подписываются и ждут изменения.
    pub watch_tx: watch::Sender<usize>,
    pub watch_rx: watch::Receiver<usize>,
}

impl Default for SharedStateInner {
    fn default() -> Self {
        let (tx, rx) = watch::channel(0usize);
        Self {
            data: Mutex::new(Inner::default()),
            watch_tx: tx,
            watch_rx: rx,
        }
    }
}

pub type SharedState = Arc<SharedStateInner>;

impl SharedStateInner {
    pub fn push(&self, mut msg: Message) {
        let next = {
            let mut d = self.data.lock().unwrap();
            msg.id = d.messages.len();
            d.messages.push(msg);
            d.messages.len()
        };
        // Уведомляем всех подписчиков — watch гарантирует доставку даже если
        // сейчас никто не ждёт (следующий .changed() вернётся сразу).
        let _ = self.watch_tx.send(next);
    }
}

pub fn new_state() -> SharedState {
    Arc::new(SharedStateInner::default())
}