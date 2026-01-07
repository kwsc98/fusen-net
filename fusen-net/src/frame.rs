use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum FrameError {
    Incomplete,
    Other(String),
}

#[derive(Debug)]
pub enum Frame {
    Ack,
    Register(Register),
    RegisterResponse(RegisterResponse),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Register {
    pub authentication: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub local_addr: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Connection {
    pub token: String,
    pub connect_time: String,
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
            0 => Frame::Ack,
            1 => Frame::Register(serde_json::from_slice(&buf[6..pointer])?),
            2 => Frame::RegisterResponse(serde_json::from_slice(&buf[6..pointer])?),
            _ => return Err(FrameError::Other("parse error".into())),
        };
        bytes.advance(pointer);
        Ok(frame)
    }

    pub fn serialization(&self) -> Result<BytesMut, FrameError> {
        let mut bytes = BytesMut::with_capacity(128);
        match self {
            Frame::Ack => {
                bytes.put_u8(0);
                bytes.extend_from_slice(b"ack");
            }
            Frame::Register(register_info) => {
                bytes.put_u8(1);
                bytes.extend_from_slice(&serde_json::to_vec(register_info)?);
            }
            Frame::RegisterResponse(register_response) => {
                bytes.put_u8(2);
                bytes.extend_from_slice(&serde_json::to_vec(register_response)?);
            }
        }
        let len = bytes.len();
        let mut head: BytesMut = BytesMut::with_capacity(5 + len);
        head.put_u8(0);
        let le_bytes = len.to_be_bytes();
        for item in le_bytes.iter().skip(4) {
            head.put_u8(*item);
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
        FrameError::Other(format!("json serialize err : {}", error))
    }
}
