use bytes::Buf;
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, io::Cursor, string::FromUtf8Error};

use crate::{buffer::Buffer, MetaData};

#[derive(Debug)]
pub enum Error {
    Incomplete,
    Other(crate::Error),
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RegisterInfo {
    tag: String,
    tcp_port: Option<String>,
    udp_port: Option<String>,
    mate_data: MetaData,
}
impl RegisterInfo {
    pub fn new(tag: String) -> Self {
        RegisterInfo {
            tag,
            tcp_port: Default::default(),
            udp_port: Default::default(),
            mate_data: Default::default(),
        }
    }

    pub fn get_tag(&self) -> &str {
        &self.tag
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionInfo {
    source_tag: String,
    target_tag: String,
    target_host: String,
}

impl ConnectionInfo {
    pub fn new(source_tag: String, target_tag: String, target_host: String) -> Self {
        ConnectionInfo {
            source_tag,
            target_tag,
            target_host,
        }
    }

    pub fn get_target_tag(&self) -> &str {
        &self.target_tag
    }
    pub fn get_source_tag(&self) -> &str {
        &self.source_tag
    }
    pub fn get_target_host(&self) -> &str {
        &self.target_host
    }
}

#[derive(Debug)]
pub enum Frame {
    Ping,
    Ack,
    KeepAlive,
    Register(RegisterInfo),
    Connection(ConnectionInfo),
    TargetConnection(ConnectionInfo),
    TargetBuffer(Buffer),
}

impl Frame {
    pub fn parse(bytes: &mut Cursor<&[u8]>) -> Result<Frame, Error> {
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

fn pop_first_u8(src: &mut Cursor<&[u8]>) -> Result<u8, Error> {
    if !src.has_remaining() {
        return Err(Error::Incomplete);
    }
    Ok(src.get_u8())
}

impl From<FromUtf8Error> for Error {
    fn from(_src: FromUtf8Error) -> Error {
        Error::Other("protocol error; invalid frame format".into())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Error {
        Error::Other(format!("json serialize err : {}", error).into())
    }
}

fn get_u16(u8_array: &[u8], start: usize) -> u16 {
    let mut lenght: u16 = u8_array[start] as u16;
    lenght <<= 8;
    lenght |= u8_array[start + 1] as u16;
    lenght
}
