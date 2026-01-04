use crate::{
    common::{ReadStream, WriteStream},
    frame::{Frame, FrameError},
};
use bytes::BytesMut;
use std::fmt::Debug;
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::error;

pub const DEFAULT_BUF_SIZE: usize = 8 * 1024;

pub struct StreamBuffer<RS, WS>
where
    RS: ReadStream,
    WS: WriteStream,
{
    recv_stream: RS,
    send_stream: WS,
    buffer: BytesMut,
    buffer_size: usize,
}

impl<RS, WS> Debug for StreamBuffer<RS, WS>
where
    RS: ReadStream,
    WS: WriteStream,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamBuffer")
            .field("recv_stream", &"...")
            .field("send_stream", &"...")
            .field("buffer", &"...")
            .field("buffer_size", &self.buffer_size)
            .finish()
    }
}

unsafe impl<RS, WS> Send for StreamBuffer<RS, WS>
where
    RS: ReadStream,
    WS: WriteStream,
{
}

impl<RS, WS> StreamBuffer<RS, WS>
where
    RS: ReadStream,
    WS: WriteStream,
{
    pub fn new(recv_stream: RS, send_stream: WS, buffer_size: usize) -> Self {
        Self {
            recv_stream,
            send_stream,
            buffer: BytesMut::with_capacity(buffer_size),
            buffer_size,
        }
    }
}

impl<RS, WS> StreamBuffer<RS, WS>
where
    RS: ReadStream,
    WS: WriteStream,
{
    async fn write_buf(&mut self, buf: &mut BytesMut) -> Result<(), io::Error> {
        let _ = self.send_stream.write_all_buf(buf).await;
        // let _ = self.send_stream.flush().await;
        Ok(())
    }

    pub async fn read_frame(&mut self) -> Result<Frame, io::Error> {
        loop {
            match Frame::parse(&mut self.buffer) {
                Ok(frame) => {
                    if self.buffer.capacity() > self.buffer_size
                        && self.buffer.len() < self.buffer_size
                    {
                        let _ = self.buffer.split_off(self.buffer_size);
                    }
                    return Ok(frame);
                }
                Err(error) => {
                    if let FrameError::Other(info) = error {
                        let msg = format!("read_frame error : {:?}", info);
                        error!(msg);
                        return Err(io::Error::other(msg));
                    }
                }
            }
            if 0 == self.recv_stream.read_buf(&mut self.buffer).await? {
                return Err(io::Error::other("connection reset by peer !"));
            }
        }
    }

    pub async fn write_frame(&mut self, frame: &Frame) -> Result<(), io::Error> {
        let mut bytes = frame
            .serialization()
            .map_err(|error| io::Error::other(format!("{:?}", error)))?;
        self.write_buf(&mut bytes).await
    }

    pub fn split(self) -> (RS, WS) {
        let StreamBuffer {
            send_stream,
            recv_stream,
            buffer: _,
            buffer_size: _,
        } = self;
        (recv_stream, send_stream)
    }
}

pub async fn connect(
    (mut r1, mut w1): (impl ReadStream, impl WriteStream),
    (mut r2, mut w2): (
        impl AsyncRead + std::marker::Unpin,
        impl AsyncWrite + std::marker::Unpin,
    ),
) -> Result<(), io::Error> {
    let _ = tokio::select! {
        res = io::copy(&mut r1, &mut w2) => res,
        res = io::copy(&mut r2, &mut w1) => res,
    };
    r1.steam_stop();
    w1.steam_stop();
    let _ = w1.shutdown().await;
    let _ = w2.shutdown().await;
    Ok(())
}
