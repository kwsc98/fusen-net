use bytes::{buf, Buf};
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, io::Cursor, string::FromUtf8Error};

use crate::{ChannelInfo, MetaData};

#[derive(Debug)]
pub enum Error {
    Incomplete,
    Other(crate::Error),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterInfo {
    tag: String,
    tcp_port: Option<String>,
    udp_port: Option<String>,
    mate_data: MetaData,
}

pub struct Data {
    target_port: u16,
    bytes: Vec<u8>,
}

impl Debug for Data {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Data")
            .field("target_port", &self.target_port)
            .field("bytes", &"...")
            .finish()
    }
}

#[derive(Debug)]
pub enum Frame {
    Ping,
    Ack,
    Register(RegisterInfo),
    SendFrame(Data),
}

impl Frame {
    pub fn parse(bytes: &mut Cursor<&[u8]>) -> Result<Frame, Error> {
        let first = pop_first_u8(bytes)?;
        if first != b'0' {
            return Err(Error::Other("parse verify error".into()));
        }
        let start = bytes.position() as usize + 1;
        let buf = bytes.get_ref();
        let end = buf.len() as usize - 1;
        if start == end {
            return Err(Error::Incomplete);
        }
        let lenght: usize = get_u16(buf, start) as usize;
        if end - start - 1 < lenght {
            return Err(Error::Incomplete);
        }
        let start = start + 2;
        let buf = &buf[start..start + lenght];
        match buf[0] {
            b'*' => {
                let target_port = get_u16(buf, 1);
                let bytes = buf[3..].to_vec();
                Frame::SendFrame(Data { target_port, bytes })
            }
            b'!' => match buf[1..buf.len()].as_ref() {
                b"ping" => Frame::Ping,
                _ => Frame::Ack,
            },
            b'+' => Frame::Register(serde_json::from_slice(&buf[1..])?),
            _ => return Err(Error::Other("parse error".into())),
        };

        todo!()
    }

    pub fn serialization(&self) -> Result<Vec<u8>, crate::Error> {
        let mut bytes = vec![];
        bytes.extend_from_slice(b"000");
        match self {
            Frame::SendFrame(data) => {
                bytes.push(b'*');
                bytes.extend_from_slice(&u16_to_u8(data.target_port));
                bytes.extend_from_slice(&data.bytes);
            }
            Frame::Ping => {
                bytes.push(b'!');
                bytes.extend_from_slice(b"ping");
            }
            Frame::Ack => {
                bytes.push(b'!');
                bytes.extend_from_slice(b"ack");
            }
            Frame::Register(register_info) => {
                bytes.push(b'+');
                bytes.extend_from_slice(serde_json::to_string(register_info)?.as_bytes());
            }
        }
        bytes.extend_from_slice(b"\r\n");
        Ok(bytes)
    }
}

fn pop_first_u8(src: &mut Cursor<&[u8]>) -> Result<u8, Error> {
    if !src.has_remaining() {
        return Err(Error::Incomplete);
    }
    return Ok(src.get_u8());
}

fn get_str(src: &mut Cursor<&[u8]>) -> Result<String, Error> {
    let start = src.position() as usize;
    let array = src.get_ref();
    let end = array.len() - 1;
    for i in start..end {
        if array[i] == b'\r' && array[i + 1] == b'\n' {
            src.set_position((i + 2) as u64);
            let str_vec = (&src.get_ref()[start..i]).to_vec();
            let string = String::from_utf8(str_vec)?;
            return Ok(string);
        }
    }
    return Err(Error::Incomplete);
}

impl From<FromUtf8Error> for Error {
    fn from(_src: FromUtf8Error) -> Error {
        return Error::Other("protocol error; invalid frame format".into());
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Error {
        return Error::Other(format!("json serialize err : {}", error.to_string()).into());
    }
}

fn get_u16(u8_array: &[u8], start: usize) -> u16 {
    let mut lenght: u16 = u8_array[start] as u16;
    lenght <<= 8;
    lenght |= u8_array[start + 1] as u16;
    lenght
}

fn u16_to_u8(u16: u16) -> [u8; 2] {
    [(u16 >> 8) as u8, u16 as u8]
}
