use tokio::{signal, sync::broadcast::{self, Sender}};

#[derive(Debug)]
pub(crate) struct Shutdown {
    shutdown: bool,

    notify: broadcast::Receiver<()>,
}

impl Shutdown {
    pub(crate) fn new(notify: broadcast::Receiver<()>) -> Shutdown {
        Shutdown {
            shutdown: false,
            notify,
        }
    }

    pub(crate) fn is_shutdown(&self) -> bool {
        self.shutdown
    }

    pub(crate) fn _shutdown(&mut self) {
        self.shutdown = true;
    }

    pub(crate) async fn recv(&mut self) {
        if self.is_shutdown() {
            return;
        }
        let _ = self.notify.recv().await;
        self.shutdown = true;
    }
}


#[derive(Debug)]
pub struct ShutdownV2 {
    shutdown: bool,
    notify: broadcast::Receiver<()>,
}

impl Default for ShutdownV2 {
    fn default() -> Self {
        let notify_shutdown: Sender<()> = broadcast::channel(1).0;
        let notify = notify_shutdown.subscribe();
        tokio::spawn(async move {
            let _ = signal::ctrl_c().await;
            drop(notify_shutdown);
        });
        ShutdownV2 {
            shutdown: false,
            notify 
        }
    }
}

impl ShutdownV2 {

    pub fn is_shutdown(&self) -> bool {
        self.shutdown
    }

    pub fn _shutdown(&mut self) {
        self.shutdown = true;
    }

    pub async fn recv(&mut self) {
        if self.is_shutdown() {
            return;
        }
        let _ = self.notify.recv().await;
        self.shutdown = true;
    }
}