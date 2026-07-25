// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use stellaris::metrics::RuntimeMetrics;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};

const MAX_CONCURRENT_SCRAPES: usize = 16;
const MAX_REQUEST_BYTES: usize = 4_096;
const SCRAPE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn serve(bind: SocketAddr, metrics: Arc<RuntimeMetrics>) -> io::Result<()> {
    let listener = TcpListener::bind(bind).await?;
    serve_listener(listener, metrics).await
}

async fn serve_listener(listener: TcpListener, metrics: Arc<RuntimeMetrics>) -> io::Result<()> {
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_SCRAPES));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let metrics = Arc::clone(&metrics);
        tokio::spawn(async move {
            let _permit = permit;
            match timeout(SCRAPE_TIMEOUT, handle_scrape(stream, metrics)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::debug!(%error, "metrics scrape failed"),
                Err(error) => tracing::debug!(%error, "metrics scrape timed out"),
            }
        });
    }
}

async fn handle_scrape(mut stream: TcpStream, metrics: Arc<RuntimeMetrics>) -> io::Result<()> {
    let mut request = [0_u8; MAX_REQUEST_BYTES];
    let mut received = 0;
    loop {
        if received == request.len() {
            return write_response(&mut stream, "431 Request Header Fields Too Large", "").await;
        }
        let count = stream.read(&mut request[received..]).await?;
        if count == 0 {
            return Ok(());
        }
        received += count;
        if request[..received]
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
            break;
        }
    }

    let first_line = request[..received]
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .map(str::trim_end);
    match first_line {
        Some("GET /metrics HTTP/1.0" | "GET /metrics HTTP/1.1") => {
            write_response(&mut stream, "200 OK", &metrics.encode_prometheus()).await
        }
        Some(line) if line.starts_with("GET ") => {
            write_response(&mut stream, "404 Not Found", "not found\n").await
        }
        Some(_) => {
            write_response(
                &mut stream,
                "405 Method Not Allowed",
                "method not allowed\n",
            )
            .await
        }
        None => write_response(&mut stream, "400 Bad Request", "bad request\n").await,
    }
}

async fn write_response(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    let content_type = if status == "200 OK" {
        "text/plain; version=0.0.4; charset=utf-8"
    } else {
        "text/plain; charset=utf-8"
    };
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_IO_TIMEOUT: Duration = Duration::from_secs(2);

    #[tokio::test]
    async fn serves_metrics_and_rejects_other_paths() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local address");
        let metrics = Arc::new(RuntimeMetrics::default());
        metrics.set_control_sessions(3);
        let server = tokio::spawn(serve_listener(listener, Arc::clone(&metrics)));

        let metrics_response =
            request(address, b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        assert!(metrics_response.starts_with("HTTP/1.1 200 OK\r\n"));
        let (headers, body) = split_response(&metrics_response);
        assert!(headers.contains("\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\n"));
        assert_eq!(content_length(headers), body.len());
        assert!(body.contains("stellaris_control_sessions 3\n"));

        metrics.set_control_sessions(4);
        let updated_response =
            request(address, b"GET /metrics HTTP/1.0\r\nHost: localhost\r\n\r\n").await;
        let (_, updated_body) = split_response(&updated_response);
        assert!(updated_body.contains("stellaris_control_sessions 4\n"));
        assert!(!updated_body.contains("stellaris_control_sessions 3\n"));

        let missing_response = request(address, b"GET /missing HTTP/1.1\r\n\r\n").await;
        assert!(missing_response.starts_with("HTTP/1.1 404 Not Found\r\n"));

        server.abort();
        let _ = server.await;
    }

    async fn request(address: SocketAddr, request: &[u8]) -> String {
        timeout(TEST_IO_TIMEOUT, async {
            let mut stream = TcpStream::connect(address).await.expect("connect");
            stream.write_all(request).await.expect("write request");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .await
                .expect("read response");
            String::from_utf8(response).expect("UTF-8 response")
        })
        .await
        .expect("metrics scrape timed out")
    }

    fn split_response(response: &str) -> (&str, &str) {
        response
            .split_once("\r\n\r\n")
            .expect("complete HTTP response")
    }

    fn content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .expect("Content-Length response header")
            .parse()
            .expect("numeric Content-Length")
    }
}
