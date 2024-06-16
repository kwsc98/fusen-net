use crate::frame::Frame;
use bytes::{Buf, BytesMut};
use quinn::{RecvStream, SendStream};
use std::fmt::Debug;
use std::io::Cursor;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufWriter},
    net::TcpStream,
};
pub struct TcpBuffer {
    stream: BufWriter<TcpStream>,
    buffer: BytesMut,
}

pub struct QuicBuffer {
    send_stream: SendStream,
    recv_stram: RecvStream,
    buffer: BytesMut,
}

impl Debug for TcpBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("stream", &"...")
            .field("buffer", &"...")
            .finish()
    }
}

impl TcpBuffer {
    pub fn new(socket: TcpStream) -> Self {
        TcpBuffer {
            stream: BufWriter::new(socket),
            buffer: BytesMut::with_capacity(4 * 1024),
        }
    }
}

impl TcpBuffer {
    pub async fn read_buf(&mut self) -> Result<&BytesMut, crate::Error> {
        self.buffer.clear();
        if 0 == self.stream.read_buf(&mut self.buffer).await? {
            return Err("connection reset by peer".into());
        }
        Ok(&self.buffer)
    }

    pub async fn write_buf(&mut self, buf: &BytesMut) -> Result<(), crate::Error> {
        self.stream.write_all(buf.chunk()).await?;
        self.stream.flush().await.map_err(|e| e.into())
    }

    pub async fn read_frame(&mut self) -> Result<Frame, crate::Error> {
        loop {
            let mut buf = Cursor::new(&self.buffer[..]);
            if let Ok(frame) = Frame::parse(&mut buf) {
                self.buffer.advance(buf.position() as usize);
                return Ok(frame);
            }
            if 0 == self.stream.read_buf(&mut self.buffer).await? {
                return Err("connection reset by peer".into());
            }
        }
    }

    pub async fn read_frame_wait(&mut self, time: Duration) -> Result<Frame, crate::Error> {
        let frame = tokio::select! {
            res = self.read_frame() => res?,
            _ = tokio::time::sleep(time) => return Err("time out".into())
        };
        Ok(frame)
    }

    pub async fn write_frame(&mut self, frame: &Frame) -> Result<(), crate::Error> {
        let mut bytes = frame.serialization()?;
        self.stream.write_all(bytes.as_mut_slice()).await?;
        self.stream.flush().await.map_err(|e| e.into())
    }
}

impl Debug for QuicBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicBuffer")
            .field("send_stream", &"...")
            .field("recv_stram", &"...")
            .field("buffer", &"...")
            .finish()
    }
}

impl QuicBuffer {
    pub fn new(send_stream: SendStream, recv_stram: RecvStream) -> Self {
        QuicBuffer {
            send_stream,
            recv_stram,
            buffer: BytesMut::with_capacity(4 * 1024),
        }
    }
}

impl QuicBuffer {
    pub async fn read_buf(&mut self) -> Result<&BytesMut, crate::Error> {
        self.buffer.clear();
        if 0 == self.recv_stram.read_buf(&mut self.buffer).await? {
            return Err("connection reset by peer".into());
        }
        Ok(&self.buffer)
    }

    pub async fn write_buf(&mut self, buf: &BytesMut) -> Result<(), crate::Error> {
        self.send_stream.write_all(buf.chunk()).await?;
        self.send_stream.flush().await.map_err(|e| e.into())
    }

    pub async fn read_frame(&mut self) -> Result<Frame, crate::Error> {
        loop {
            let mut buf = Cursor::new(&self.buffer[..]);
            if let Ok(frame) = Frame::parse(&mut buf) {
                self.buffer.advance(buf.position() as usize);
                return Ok(frame);
            }
            if 0 == self.recv_stram.read_buf(&mut self.buffer).await? {
                return Err("connection reset by peer".into());
            }
        }
    }

    pub async fn read_frame_wait(&mut self, time: Duration) -> Result<Frame, crate::Error> {
        let frame = tokio::select! {
            res = self.read_frame() => res?,
            _ = tokio::time::sleep(time) => return Err("time out".into())
        };
        Ok(frame)
    }

    pub async fn write_frame(&mut self, frame: &Frame) -> Result<(), crate::Error> {
        let mut bytes = frame.serialization()?;
        self.send_stream.write_all(bytes.as_mut_slice()).await?;
        self.send_stream.flush().await.map_err(|e| e.into())
    }
}
