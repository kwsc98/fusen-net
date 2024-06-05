use crate::frame::Frame;
use bytes::{Buf, BytesMut};
use std::io::Cursor;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{io::BufWriter, net::TcpStream};
use tracing::info;

pub struct Buffer {
    stream: BufWriter<TcpStream>,
    buffer: BytesMut,
}

impl Buffer {
    pub fn new(socket: TcpStream) -> Self {
        return Buffer {
            stream: BufWriter::new(socket),
            buffer: BytesMut::with_capacity(4 * 1024),
        };
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
