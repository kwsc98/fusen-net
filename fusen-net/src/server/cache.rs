use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::oneshot;

enum CacheSender<K, V> {
    GET(K),
    INSERT((K, V)),
    REMOVE(K),
}

enum CacheReceiver<V> {
    GET(Option<V>),
    INSERT(Option<V>),
    REMOVE(Option<V>),
}

#[derive(Clone)]
pub struct AsyncCache<K, V> {
    _k: PhantomData<K>,
    _v: PhantomData<V>,
    sender: UnboundedSender<(CacheSender<K, V>, oneshot::Sender<CacheReceiver<V>>)>,
}

impl<K, V> AsyncCache<K, V>
where
    K: Hash + Eq + std::marker::Send + Sync + 'static,
    V: std::marker::Send + Sync + 'static + Clone,
{
    pub fn new() -> Self {
        let (sender, mut receiver) =
            mpsc::unbounded_channel::<(CacheSender<K, V>, oneshot::Sender<CacheReceiver<V>>)>();
        tokio::spawn(async move {
            let mut map = HashMap::<K, V>::new();
            while let Some(msg) = receiver.recv().await {
                match msg.0 {
                    CacheSender::GET(key) => {
                        let _ = msg.1.send(CacheReceiver::GET(map.get(&key).cloned()));
                    }
                    CacheSender::INSERT((key, value)) => {
                        let value = map.insert(key, value);
                        let _ = msg.1.send(CacheReceiver::INSERT(value));
                    }
                    CacheSender::REMOVE(key) => {
                        let value = map.remove(&key);
                        let _ = msg.1.send(CacheReceiver::REMOVE(value));
                    }
                }
            }
        });
        Self {
            _k: PhantomData,
            _v: PhantomData,
            sender,
        }
    }

    pub async fn get(&self, key: K) -> Result<Option<V>, crate::Error> {
        let oneshot = oneshot::channel();
        let _ = self.sender.send((CacheSender::GET(key), oneshot.0));
        match oneshot.1.await? {
            CacheReceiver::GET(value) => Ok(value),
            _ => Err("err receiver".into()),
        }
    }

    pub async fn insert(&self, key: K, value: V) -> Result<Option<V>, crate::Error> {
        let oneshot = oneshot::channel();
        let _ = self
            .sender
            .send((CacheSender::INSERT((key, value)), oneshot.0));
        match oneshot.1.await? {
            CacheReceiver::INSERT(value) => Ok(value),
            _ => Err("err receiver".into()),
        }
    }

    pub async fn remove(&self, key: K) -> Result<Option<V>, crate::Error> {
        let oneshot = oneshot::channel();
        let _ = self.sender.send((CacheSender::REMOVE(key), oneshot.0));
        match oneshot.1.await? {
            CacheReceiver::REMOVE(value) => Ok(value),
            _ => Err("err receiver".into()),
        }
    }
}
