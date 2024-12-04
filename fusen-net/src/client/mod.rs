use std::time::Duration;

use crate::{
    frame::{Frame, RegisterInfo},
    quic::support::make_client_endpoint,
};
use base64::Engine;
use channel::handler;
use fusen_common::BoxError;
use quinn::{Connection, Endpoint};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot,
};
use tracing::error;
mod channel;

pub struct Agent {
    sender: UnboundedSender<(Frame, oneshot::Sender<Result<(), BoxError>>)>,
}

impl Agent {
    pub async fn new(
        register: &str,
        server_cert: &str,
        server_name: &str,
    ) -> Result<Self, BoxError> {
        let endpoint = make_client_endpoint(
            "0.0.0.0:0".parse().unwrap(),
            vec![base64::prelude::BASE64_STANDARD
                .decode(server_cert)?
                .as_slice()]
            .as_slice(),
        )?;
        let connection = endpoint.connect(register.parse()?, server_name)?.await?;

        let (sender, recv) =
            mpsc::unbounded_channel::<(Frame, oneshot::Sender<Result<(), BoxError>>)>();
        let (conn_sender, conn_recv) =
            mpsc::unbounded_channel::<oneshot::Sender<Result<Connection, BoxError>>>();
        let register = register.to_owned();
        let server_name = server_name.to_owned();
        tokio::spawn(async move {
            let _ = connect_handler(conn_recv, endpoint, &register, &server_name, connection).await;
        });
        tokio::spawn(async move {
            let _ = handler(recv, conn_sender).await;
        });
        Ok(Agent { sender })
    }

    pub async fn register(&self, register: RegisterInfo) -> Result<(), BoxError> {
        self.send_frame(Frame::Register(register)).await
    }

    pub async fn unregister(&self, register: RegisterInfo) -> Result<(), BoxError> {
        self.send_frame(Frame::UnRegister(register)).await
    }

    async fn send_frame(&self, frame: Frame) -> Result<(), BoxError> {
        let (oneshot_sender, oneshot_recv) = oneshot::channel::<Result<(), BoxError>>();
        self.sender.send((frame, oneshot_sender))?;
        oneshot_recv.await?
    }
}

pub async fn get_connection(
    sender: &UnboundedSender<oneshot::Sender<Result<Connection, BoxError>>>,
) -> Result<Connection, BoxError> {
    let (oneshot_sender, oneshot_recv) = oneshot::channel();
    sender.send(oneshot_sender)?;
    oneshot_recv.await?
}

pub async fn connect_handler(
    mut recv: UnboundedReceiver<oneshot::Sender<Result<Connection, BoxError>>>,
    endpoint: Endpoint,
    register: &str,
    server_name: &str,
    connection: Connection,
) -> Result<(), BoxError> {
    let mut connect = connection;
    loop {
        tokio::select! {
            sender = recv.recv() => {
                if sender.is_none() {
                   error!("agent close!");
                   return Ok(());
                }
                let _ = sender.unwrap().send(Ok(connect.clone()));
            },
            error = connect.closed() => {
                error!("connect close ! : {:?}",error);
                match endpoint.connect(register.parse().unwrap(), server_name).unwrap().await {
                    Ok(new_connect) => {
                        connect = new_connect;
                    },
                    Err(error) => {
                        error!("retry connect error ! : {:?}",error);
                        let _ = tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                };
            }
        }
    }
}
