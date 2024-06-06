use std::{net::TcpStream, sync::Arc};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    buffer::Buffer,
    frame::{ConnectionInfo, Frame},
    ChannelInfo,
};

pub async fn connect(mut buf1: Buffer, mut buf2: Buffer) -> Result<(), crate::Error> {
    let tcp_stream : TcpStream = buf1.;
    Ok(())
}

pub async fn handler(
    connection_info: ConnectionInfo,
    mut buffer1: Buffer,
    channel_info: Arc<ChannelInfo>,
    mut receiver: UnboundedReceiver<Frame>,
) -> Result<(), crate::Error> {
    channel_info
        .sender
        .send(Frame::Connection(connection_info))?;
    let frame = receiver.recv().await.ok_or("receive error")?;
    let Frame::TargetBuffer(buffer2) = frame else {
        return Err("receive error frame".into());
    };
    let _ = buffer1.write_frame(&Frame::Ack).await;
    connect(buffer1, buffer2).await
}
