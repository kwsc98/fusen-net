use crate::buffer::Buffer;

pub async fn connect(mut buf1: Buffer, mut buf2: Buffer) -> Result<(), crate::Error> {
    loop {
        tokio::select! {
            res1 = buf1.read_buf() => {
                buf2.write_buf(res1?).await?;
            },
            res2 = buf2.read_buf() => {
                buf1.write_buf(res2?).await?;
            },
        }
    }
}
