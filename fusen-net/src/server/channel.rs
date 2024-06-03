use std::sync::Arc;

use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use super::cache::AsyncCache;

pub struct Channel {
    stream: TcpStream,
    socket_addr: SocketAddr,
    async_cache: AsyncCache<String, Arc<SocketAddr>>,
    shutdown_complete_tx: mpsc::Sender<()>,
}

impl Channel {
    pub fn new(
        stream: TcpStream,
        socket_addr: SocketAddr,
        async_cache: AsyncCache<String, Arc<SocketAddr>>,
        shutdown_complete_tx: mpsc::Sender<()>,
    ) -> Self {
        Self {
            stream,
            socket_addr,
            async_cache,
            shutdown_complete_tx,
        }
    }

    pub async fn run(self) {
        let Channel {
            stream,
            socket_addr,
            async_cache,
            shutdown_complete_tx,
        } = self;
        let buffer = Buff
        
    }
}
