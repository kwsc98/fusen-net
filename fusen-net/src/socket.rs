use tokio::net::TcpStream;

pub async fn get_tcp_stream(ip: String, port: u16) -> Result<TcpStream, crate::Error> {
    let addr = format!("{}:{}", ip, port);
    Ok(TcpStream::connect(addr).await?)
}

pub async fn get_tcp_stream_by_host(host: String) -> Result<TcpStream, crate::Error> {
    Ok(TcpStream::connect(host).await?)
}
