use crate::buffer::{QuicBuffer, TcpBuffer};

pub async fn connect_tcp_to_quic(mut buf1: TcpBuffer, mut buf2: QuicBuffer) -> Result<(), crate::Error> {
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

pub async fn connect_quic_to_quic(mut buf1: QuicBuffer, mut buf2: QuicBuffer) -> Result<(), crate::Error> {
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
