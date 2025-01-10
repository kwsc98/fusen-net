use bytes::{Buf, BufMut, BytesMut};
use fusen_common::fusen_procedural_macro::Data;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum FrameError {
    Incomplete,
    Other(fusen_common::BoxError),
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Frame {
    Ping,
    Ack(String),
    Register(RegisterInfo),
    Connection(ConnectionInfo),
    TargetConnection(ConnectionInfo),
}

#[derive(Default, Debug, Deserialize, Serialize, Clone, Data)]
pub struct RegisterInfo {
    info: String,
    //0 tcp 1 udp
    protocol: u16,
    remote_port: Option<u16>,
    target_host: String,
}

#[derive(Debug, Deserialize, Serialize, Data, Default)]
pub struct ConnectionInfo {
    uuid: String,
    target_host: String,
}

impl Frame {
    pub fn parse(bytes: &mut BytesMut) -> Result<Frame, FrameError> {
        let buf = bytes.as_ref();
        let Some(first) = buf.first() else {
            return Err(FrameError::Incomplete);
        };
        if first != &b'0' {
            return Err(FrameError::Other("parse verify error".into()));
        }
        let end = buf.len();
        if end < 5 {
            return Err(FrameError::Incomplete);
        }
        let lenght: usize = get_context_len(buf, 1);
        if lenght + 5 > end {
            return Err(FrameError::Incomplete);
        }
        let pointer = 5 + lenght;
        let frame = match buf[5] {
            b'*' => Frame::Connection(serde_json::from_slice(&buf[6..pointer])?),
            b'&' => Frame::TargetConnection(serde_json::from_slice(&buf[6..pointer])?),
            b'+' => Frame::Register(serde_json::from_slice(&buf[6..pointer])?),
            b'!' => match &buf[6..pointer] {
                b"ping" => Frame::Ping,
                _ => Frame::Ack(serde_json::from_slice(&buf[6..pointer])?),
            },
            _ => return Err(FrameError::Other("parse error".into())),
        };
        bytes.advance(pointer);
        Ok(frame)
    }

    pub fn serialization(&self) -> Result<BytesMut, crate::Error> {
        let mut bytes = BytesMut::with_capacity(128);
        match self {
            Frame::Connection(connection_info) => {
                bytes.put_u8(b'*');
                bytes.extend_from_slice(&serde_json::to_vec(connection_info)?);
            }
            Frame::TargetConnection(connection_info) => {
                bytes.put_u8(b'&');
                bytes.extend_from_slice(&serde_json::to_vec(connection_info)?);
            }
            Frame::Ping => {
                bytes.put_u8(b'!');
                bytes.extend_from_slice(b"ping");
            }
            Frame::Ack(msg) => {
                bytes.put_u8(b'!');
                bytes.extend_from_slice(&serde_json::to_vec(msg)?);
            }
            Frame::Register(register_info) => {
                bytes.put_u8(b'+');
                bytes.extend_from_slice(&serde_json::to_vec(register_info)?);
            }
        }
        let len = bytes.len();
        let mut head: BytesMut = BytesMut::with_capacity(5 + len);
        head.put_u8(b'0');
        let le_bytes = len.to_le_bytes();
        for idx in (0..4).rev() {
            head.put_u8(le_bytes[idx]);
        }
        head.unsplit(bytes);
        Ok(head)
    }
}

fn get_context_len(u8_array: &[u8], start: usize) -> usize {
    let mut lenght: usize = 0;
    for idx in 0..4 {
        lenght <<= 8;
        lenght |= u8_array[start + idx] as usize;
    }
    lenght
}

impl From<serde_json::Error> for FrameError {
    fn from(error: serde_json::Error) -> FrameError {
        FrameError::Other(format!("json serialize err : {}", error).into())
    }
}

#[test]
fn test() {
    let frame = Frame::Ack("ok".to_owned());
    let bytes = frame.serialization().unwrap();
    let mut bytes_mut = BytesMut::new();
    bytes_mut.extend_from_slice(bytes.as_ref());
    println!("{:?}", bytes_mut);
    let frame_copy = Frame::parse(&mut bytes_mut).unwrap();
    println!("{:?}", frame_copy);
    println!("{:?}", bytes_mut);
}
