use std::io::Cursor;

use bytes::Buf;
use fusen_common::BoxError;

#[derive(Debug)]
pub enum Frame {
    Ping(u64),
    Ack(u64),
    Register(RegisterInfo),
    Connection(ConnectionInfo),
    TargetConnection(ConnectionInfo),
}

#[derive(Debug)]
pub struct RegisterInfo {
    uuid: String,
    info: String,
    //0 tcp 1 udp
    protocol: u16,
    target_host: Option<Vec<String>>,
    remote_port: u16,
}

#[derive(Debug)]
pub struct ConnectionInfo {
    uuid: String,
    target_host: String,
}

impl Frame {
    pub fn parse(bytes: &mut Cursor<&[u8]>) -> Result<Frame, BoxError> {
        let first = pop_first_u8(bytes)?;
        if first != b'0' {
            return Err(Error::Other("parse verify error".into()));
        }
        let start = bytes.position() as usize;
        let buf = bytes.get_ref();
        let end = buf.len();
        if start + 1 >= end {
            return Err(Error::Incomplete);
        }
        let lenght: usize = get_u16(buf, start) as usize;
        if start + lenght + 2 > end {
            return Err(Error::Incomplete);
        }
        let start = start + 2;
        let buf = &buf[start..start + lenght];
        bytes.set_position((start + lenght) as u64);
        let frame = match buf[0] {
            b'*' => Frame::Connection(serde_json::from_slice(&buf[1..])?),
            b'&' => Frame::TargetConnection(serde_json::from_slice(&buf[1..])?),
            b'^' => Frame::Subscribe(serde_json::from_slice(&buf[1..])?),
            b'!' => match buf[1..buf.len()].as_ref() {
                b"ping" => Frame::Ping,
                b"keepalive" => Frame::KeepAlive,
                _ => Frame::Ack,
            },
            b'+' => Frame::Register(serde_json::from_slice(&buf[1..])?),
            _ => return Err(Error::Other("parse error".into())),
        };
        Ok(frame)
    }

    pub fn serialization(&self) -> Result<Vec<u8>, crate::Error> {
        let mut bytes = vec![];
        bytes.extend_from_slice(b"000");
        match self {
            Frame::Connection(connection_info) => {
                bytes.push(b'*');
                bytes.extend_from_slice(serde_json::to_string(connection_info)?.as_bytes());
            }
            Frame::Subscribe(connection_info) => {
                bytes.push(b'^');
                bytes.extend_from_slice(serde_json::to_string(connection_info)?.as_bytes());
            }
            Frame::TargetConnection(connection_info) => {
                bytes.push(b'&');
                bytes.extend_from_slice(serde_json::to_string(connection_info)?.as_bytes());
            }
            Frame::Ping => {
                bytes.push(b'!');
                bytes.extend_from_slice(b"ping");
            }
            Frame::Ack => {
                bytes.push(b'!');
                bytes.extend_from_slice(b"ack");
            }
            Frame::KeepAlive => {
                bytes.push(b'!');
                bytes.extend_from_slice(b"keepalive");
            }
            Frame::Register(register_info) => {
                bytes.push(b'+');
                bytes.extend_from_slice(serde_json::to_string(register_info)?.as_bytes());
            }
            _ => return Err("serialization error".into()),
        }
        let length = (bytes.len() - 3) as u16;
        bytes[1] = (length >> 8) as u8;
        bytes[2] = length as u8;
        Ok(bytes)
    }
}

fn pop_first_u8(src: &mut Cursor<&[u8]>) -> Result<u8, BoxError> {
    if !src.has_remaining() {
        return Err(Error::Incomplete);
    }
    Ok(src.get_u8())
}

fn get_u16(u8_array: &[u8], start: usize) -> u16 {
    let mut lenght: u16 = u8_array[start] as u16;
    lenght <<= 8;
    lenght |= u8_array[start + 1] as u16;
    lenght
}
