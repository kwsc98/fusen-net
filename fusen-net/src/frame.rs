use std::{io::Cursor, string::FromUtf8Error};

use bytes::Buf;

use crate::ChannelInfo;

#[derive(Debug)]
pub enum Error {
    Incomplete,
    Other(crate::Error),
}

#[derive(Debug)]
pub enum Frame {
    PING,
    ACK,
    ERROR,
    NotFind,
    Register(ChannelInfo),
    Subscribe(String),
}

impl Frame {
    pub fn parse(src: &mut Cursor<&[u8]>) -> Result<Frame, Error> {
        let first = pop_first_u8(src)?;
        let data_str = get_str(src)?;
        Ok(match first {
            b'+' => Frame::Register(serde_json::from_str(&data_str)?),
            b'$' => Frame::Subscribe(data_str),
            b'*' => match data_str.as_str() {
                "ack" => Frame::ACK,
                "error" => Frame::ERROR,
                "notfind" => Frame::NotFind,
                _ => Frame::PING,
            },
            u8 => return Err(Error::Other(format!("'{}' not support", u8).into())),
        })
    }

    pub fn serialization(&self) -> Result<Vec<u8>, crate::Error> {
        let mut bytes = vec![];
        match self {
            Frame::PING => {
                bytes.push(b'*');
                bytes.extend_from_slice(b"ping");
            }
            Frame::ACK => {
                bytes.push(b'*');
                bytes.extend_from_slice(b"ack");
            }
            Frame::ERROR => {
                bytes.push(b'*');
                bytes.extend_from_slice(b"error");
            }
            Frame::NotFind => {
                bytes.push(b'*');
                bytes.extend_from_slice(b"notfind");
            }
            Frame::Register(channel_info) => {
                bytes.push(b'+');
                bytes.extend_from_slice(serde_json::to_string(channel_info)?.as_bytes());
            }
            Frame::Subscribe(tag) => {
                bytes.push(b'$');
                bytes.extend_from_slice(tag.as_bytes());
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
