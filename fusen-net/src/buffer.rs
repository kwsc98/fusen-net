use std::pin::Pin;

use crate::frame::{Frame, FrameError};
use bytes::BytesMut;
use fusen_common::{shutdown::Shutdown, BoxError};
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{error, info};

pub const DEFAULT_BUF_SIZE: usize = 8 * 1024;

#[allow(async_fn_in_trait)]
pub trait Buffer {
    async fn read_buf(&mut self) -> Result<&mut BytesMut, BoxError>;

    async fn write_buf(&mut self, buf: &mut BytesMut) -> Result<(), BoxError>;

    async fn read_frame(&mut self) -> Result<Frame, BoxError>;

    async fn write_frame(&mut self, frame: &Frame) -> Result<(), BoxError>;

    fn split(
        self,
    ) -> (
        Pin<Box<dyn AsyncRead + Send>>,
        Pin<Box<dyn AsyncWrite + Send>>,
    );
}

pub struct QuicBuffer {
    recv_stream: Pin<Box<dyn AsyncRead + Send>>,
    send_stream: Pin<Box<dyn AsyncWrite + Send>>,
    buffer: BytesMut,
    buffer_size: usize,
}

unsafe impl Send for QuicBuffer {}

impl QuicBuffer {
    pub fn new(
        recv_stream: impl AsyncRead + 'static + Send,
        send_stream: impl AsyncWrite + 'static + Send,
        buffer_size: usize,
    ) -> Self {
        Self {
            recv_stream: Box::pin(recv_stream),
            send_stream: Box::pin(send_stream),
            buffer: BytesMut::with_capacity(buffer_size),
            buffer_size,
        }
    }
}

impl Buffer for QuicBuffer {
    async fn read_buf(&mut self) -> Result<&mut BytesMut, BoxError> {
        if 0 == self.recv_stream.read_buf(&mut self.buffer).await? {
            return Err("connection reset by peer".into());
        }
        if self.buffer.capacity() > self.buffer_size && self.buffer.len() < self.buffer_size {
            let _ = self.buffer.split_off(self.buffer_size);
        }
        Ok(&mut self.buffer)
    }

    async fn write_buf(&mut self, buf: &mut BytesMut) -> Result<(), BoxError> {
        self.send_stream.write_all_buf(buf).await?;
        self.send_stream.flush().await.map_err(|e| e.into())
    }

    async fn read_frame(&mut self) -> Result<Frame, BoxError> {
        loop {
            match Frame::parse(&mut self.buffer) {
                Ok(frame) => return Ok(frame),
                Err(error) => {
                    if let FrameError::Other(info) = error {
                        error!("read_frame error : {:?}", info);
                        return Err(info);
                    }
                }
            }
            if 0 == self.recv_stream.read_buf(&mut self.buffer).await? {
                return Err("connection reset by peer".into());
            }
        }
    }

    async fn write_frame(&mut self, frame: &Frame) -> Result<(), BoxError> {
        let mut bytes = frame.serialization()?;
        self.write_buf(&mut bytes).await
    }

    fn split(
        self,
    ) -> (
        Pin<Box<dyn AsyncRead + Send>>,
        Pin<Box<dyn AsyncWrite + Send>>,
    ) {
        let QuicBuffer {
            send_stream,
            recv_stream,
            buffer: _,
            buffer_size: _,
        } = self;
        (recv_stream, send_stream)
    }
}

pub async fn connect(
    (mut r1, mut w1): (Pin<Box<dyn AsyncRead + Send>>, Pin<Box<dyn AsyncWrite+ Send>>),
    (mut r2, mut w2): (
        impl AsyncRead + std::marker::Unpin,
        impl AsyncWrite + std::marker::Unpin,
    ),
) -> Result<(), BoxError> {
    let _ = tokio::select! {
        res = io::copy(&mut r1, &mut w2) => res,
        res = io::copy(&mut r2, &mut w1) => res,
    };
    Ok(())
}
