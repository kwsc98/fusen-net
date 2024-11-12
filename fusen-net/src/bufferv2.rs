use bytes::Bytes;
use fusen_common::BoxError;

pub trait Buffer {
    async fn read_buf(&mut self) -> Result<Bytes, BoxError>;

    async fn write_buf(&mut self) -> Result<Bytes, BoxError>;

}
