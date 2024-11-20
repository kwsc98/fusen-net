use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::oneshot;

use crate::buffer::QuicBuffer;

enum CacheSender {
    Insert((String, oneshot::Sender<QuicBuffer>)),
    Remove(String),
}

enum CacheReceiver {
    Insert(Option<oneshot::Sender<QuicBuffer>>),
    Remove(Option<oneshot::Sender<QuicBuffer>>),
}
type AsyncMapSender = UnboundedSender<(CacheSender, oneshot::Sender<CacheReceiver>)>;

#[derive(Clone)]
pub struct AsyncQuicBufferMap {
    sender: Arc<AsyncMapSender>,
}

impl Default for AsyncQuicBufferMap {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncQuicBufferMap {
    pub fn new() -> Self {
        let (sender, mut receiver) =
            mpsc::unbounded_channel::<(CacheSender, oneshot::Sender<CacheReceiver>)>();
        tokio::spawn(async move {
            let mut map = HashMap::new();
            while let Some(msg) = receiver.recv().await {
                match msg.0 {
                    CacheSender::Insert((key, value)) => {
                        let value = map.insert(key, value);
                        let _ = msg.1.send(CacheReceiver::Insert(value));
                    }
                    CacheSender::Remove(key) => {
                        let value = map.remove(&key);
                        let _ = msg.1.send(CacheReceiver::Remove(value));
                    }
                }
            }
        });
        Self {
            sender: Arc::new(sender),
        }
    }

    pub async fn insert(
        &self,
        key: String,
        value: oneshot::Sender<QuicBuffer>,
    ) -> Option<oneshot::Sender<QuicBuffer>> {
        let oneshot = oneshot::channel();
        let _ = self
            .sender
            .send((CacheSender::Insert((key, value)), oneshot.0));
        match oneshot.1.await.unwrap() {
            CacheReceiver::Insert(value) => value,
            _ => panic!("err receiver"),
        }
    }

    pub async fn remove(&self, key: String) -> Option<oneshot::Sender<QuicBuffer>> {
        let oneshot = oneshot::channel();
        let _ = self.sender.send((CacheSender::Remove(key), oneshot.0));
        match oneshot.1.await.unwrap() {
            CacheReceiver::Remove(value) => value,
            _ => panic!("err receiver"),
        }
    }
}

#[tokio::test]
async fn test() {
    let map: AsyncQuicBufferMap = AsyncQuicBufferMap::new();
    let _ = map.clone();
}
