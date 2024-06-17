use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::oneshot;

enum CacheSender<K, V> {
    Get(K),
    Insert((K, V)),
    Remove(K),
}

enum CacheReceiver<V> {
    Get(Option<V>),
    Insert(Option<V>),
    Remove(Option<V>),
}
type AsyncCacheSender<K, V> =
    UnboundedSender<(CacheSender<K, V>, oneshot::Sender<CacheReceiver<V>>)>;

#[derive(Clone)]
pub struct AsyncCache<K, V> {
    _k: PhantomData<K>,
    _v: PhantomData<V>,
    sender: AsyncCacheSender<K, V>,
}

impl<K, V> Default for AsyncCache<K, V>
where
    K: Hash + Eq + std::marker::Send + Sync + 'static,
    V: std::marker::Send + Sync + 'static + Clone,
{
    fn default() -> Self {
        Self::new()
    }
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
                    CacheSender::Get(key) => {
                        let _ = msg.1.send(CacheReceiver::Get(map.get(&key).cloned()));
                    }
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
            _k: PhantomData,
            _v: PhantomData,
            sender,
        }
    }

    pub async fn get(&self, key: K) -> Result<Option<V>, crate::Error> {
        let oneshot = oneshot::channel();
        let _ = self.sender.send((CacheSender::Get(key), oneshot.0));
        match oneshot.1.await? {
            CacheReceiver::Get(value) => Ok(value),
            _ => Err("err receiver".into()),
        }
    }

    pub async fn insert(&self, key: K, value: V) -> Result<Option<V>, crate::Error> {
        let oneshot = oneshot::channel();
        let _ = self
            .sender
            .send((CacheSender::Insert((key, value)), oneshot.0));
        match oneshot.1.await? {
            CacheReceiver::Insert(value) => Ok(value),
            _ => Err("err receiver".into()),
        }
    }

    pub async fn remove(&self, key: K) -> Result<Option<V>, crate::Error> {
        let oneshot = oneshot::channel();
        let _ = self.sender.send((CacheSender::Remove(key), oneshot.0));
        match oneshot.1.await? {
            CacheReceiver::Remove(value) => Ok(value),
            _ => Err("err receiver".into()),
        }
    }
}
