use crate::frame::{Frame, FrameError};
use bytes::BytesMut;
use fusen_common::BoxError;
use quinn::{RecvStream, SendStream};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufWriter},
    net::TcpStream,
};
use tracing::error;

pub trait Buffer {
    async fn read_buf(&mut self) -> Result<&mut BytesMut, BoxError>;

    async fn write_buf(&mut self, buf: &mut BytesMut) -> Result<(), BoxError>;

    async fn read_frame(&mut self) -> Result<Frame, BoxError>;

    async fn write_frame(&mut self, frame: &Frame) -> Result<(), BoxError>;
}

pub struct QuicBuffer {
    send_stream: SendStream,
    recv_stram: RecvStream,
    buffer: BytesMut,
    buffer_size: usize,
}

impl QuicBuffer {
    pub fn new(send_stream: SendStream, recv_stram: RecvStream, buffer_size: usize) -> Self {
        Self {
            send_stream,
            recv_stram,
            buffer: BytesMut::with_capacity(buffer_size),
            buffer_size,
        }
    }
}

impl Buffer for QuicBuffer {
    async fn read_buf(&mut self) -> Result<&mut BytesMut, BoxError> {
        if 0 == self.recv_stram.read_buf(&mut self.buffer).await? {
            return Err("connection reset by peer".into());
        }
        if self.buffer.capacity() > self.buffer_size && self.buffer.len() < self.buffer_size {
            self.buffer.split_off(self.buffer_size);
        }
        Ok(&mut self.buffer)
    }

    async fn write_buf(&mut self, buf: &mut BytesMut) -> Result<(), BoxError> {
        self.send_stream.write_chunk(buf.split().freeze()).await?;
        self.send_stream.flush().await.map_err(|e| e.into())
    }

    async fn read_frame(&mut self) -> Result<Frame, BoxError> {
        loop {
            match Frame::parse(&mut self.buffer) {
                Ok(frame) => return Ok(frame),
                Err(error) => {
                    if let FrameError::Other(info) = error {
                        error!("read_frame error : {:?}", info);
                        return Err(info.into());
                    }
                }
            }
            if 0 == self.recv_stram.read_buf(&mut self.buffer).await? {
                return Err("connection reset by peer".into());
            }
        }
    }

    async fn write_frame(&mut self, frame: &Frame) -> Result<(), BoxError> {
        let mut bytes = frame.serialization()?;
        self.write_buf(&mut bytes).await
    }
}

pub struct TcpBuffer {
    stream: BufWriter<TcpStream>,
    buffer: BytesMut,
    buffer_size: usize,
}

impl TcpBuffer {
    pub fn new(stream: TcpStream, buffer_size: usize) -> Self {
        Self {
            stream: BufWriter::new(stream),
            buffer: BytesMut::with_capacity(buffer_size),
            buffer_size,
        }
    }
}

impl Buffer for TcpBuffer {
    async fn read_buf(&mut self) -> Result<&mut BytesMut, BoxError> {
        if 0 == self.stream.read_buf(&mut self.buffer).await? {
            return Err("connection reset by peer".into());
        }
        if self.buffer.capacity() > self.buffer_size && self.buffer.len() < self.buffer_size {
            self.buffer.split_off(self.buffer_size);
        }
        Ok(&mut self.buffer)
    }

    async fn write_buf(&mut self, buf: &mut BytesMut) -> Result<(), BoxError> {
        self.stream.write_buf(buf).await?;
        self.stream.flush().await.map_err(|e| e.into())
    }

    async fn read_frame(&mut self) -> Result<Frame, BoxError> {
        loop {
            match Frame::parse(&mut self.buffer) {
                Ok(frame) => return Ok(frame),
                Err(error) => {
                    if let FrameError::Other(info) = error {
                        error!("read_frame error : {:?}", info);
                        return Err(info.into());
                    }
                }
            }
            if 0 == self.stream.read_buf(&mut self.buffer).await? {
                return Err("connection reset by peer".into());
            }
        }
    }

    async fn write_frame(&mut self, frame: &Frame) -> Result<(), BoxError> {
        let mut bytes = frame.serialization()?;
        self.write_buf(&mut bytes).await
    }
}
