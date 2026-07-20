use std::{
    collections::VecDeque,
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker, ready},
};

use bytes::{BufMut, Bytes};
use qbase::{
    error::{Error, ErrorKind, QuicError},
    frame::{DatagramFrame, EncodeSize, GetFrameType},
};

use crate::{DATAGRAM_QUEUE_CAPACITY, lock_unpoisoned};

#[derive(Debug)]
struct RawDatagarmReader {
    local_max_size: usize,
    rcvd_datagrams: VecDeque<Bytes>,
    dropped_queue_full: u64,
    read_waker: Option<Waker>,
}

impl RawDatagarmReader {
    fn new(local_max_size: usize) -> Self {
        Self {
            local_max_size,
            rcvd_datagrams: VecDeque::with_capacity(DATAGRAM_QUEUE_CAPACITY),
            dropped_queue_full: 0,
            read_waker: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DatagramIncoming(Arc<Mutex<Result<RawDatagarmReader, Error>>>);

impl DatagramIncoming {
    /// Create a new [`DatagramIncoming`] to receive datagram frames.
    pub fn new(local_max_size: usize) -> Self {
        Self(Arc::new(Mutex::new(Ok(RawDatagarmReader::new(
            local_max_size,
        )))))
    }

    /// Try to create a new [`DatagramReader`] for the application to read the received datagram frames.
    ///
    /// Returns an error when the Unreliable Datagram Extension was disenabled by local parameters,
    /// see <https://www.rfc-editor.org/rfc/rfc9221.html#name-transport-parameter> for more delails.
    pub fn new_reader(&self) -> io::Result<DatagramReader> {
        let mut guard = lock_unpoisoned(&self.0);
        let reader = guard.as_mut().map_err(|e| e.clone())?;
        if reader.local_max_size == 0 {
            tracing::error!("   Cause by: DatagramIncoming::new_reader local_max_size is 0");
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unreliable Datagram Extension was disenabled by local parameters",
            ));
        }

        Ok(DatagramReader(self.0.clone()))
    }

    /// Receives a datagram frame for the application to read.
    ///
    /// If the size of the received datagram exceeds the maximum size set by the local protocol parameters `max_datagram_frame_size`,
    /// a connection error occurs.
    ///
    /// If the connection is closing or closed, the new datagram will be ignored.
    ///
    /// If the application is waiting for the data to be read, the task will be woken up when the datagram is received.
    /// If the bounded receive queue is full, the new Datagram is dropped.
    pub fn recv_datagram(&self, frame: &DatagramFrame, data: bytes::Bytes) -> Result<(), Error> {
        let mut guard = lock_unpoisoned(&self.0);
        let reader = guard.as_mut().map_err(|e| e.clone())?;
        if (frame.encoding_size() + data.len()) > reader.local_max_size {
            tracing::error!("   Cause by: DatagramIncoming::recv_datagram");
            return Err(QuicError::new(
                ErrorKind::ProtocolViolation,
                frame.frame_type().into(),
                format!(
                    "datagram size {} exceeds the maximum size {}",
                    frame.encoding_size() + data.len(),
                    reader.local_max_size
                ),
            )
            .into());
        }
        if reader.rcvd_datagrams.len() >= DATAGRAM_QUEUE_CAPACITY {
            reader.dropped_queue_full = reader.dropped_queue_full.saturating_add(1);
            return Ok(());
        }

        reader.rcvd_datagrams.push_back(data);
        if let Some(waker) = reader.read_waker.take() {
            waker.wake();
        }

        Ok(())
    }

    /// When a connection error occurs, the error will be set to the reader.
    ///
    /// Any subsequent calls to [`DatagramIncoming::new_reader`], [`DatagramReader::poll_recv`], [`DatagramReader::read`]
    /// and [`DatagramReader::read_buf`] will return an error.
    ///
    /// If there is a task waiting for the data to be read, the task will be woken up and return an error immediately.
    ///
    /// All the received datagrams will be discarded, and subsequent calls to [`DatagramIncoming::recv_datagram`] will be ignored.
    pub fn on_conn_error(&self, error: &Error) {
        let guard = &mut lock_unpoisoned(&self.0);
        if let Ok(reader) = guard.as_mut() {
            if let Some(waker) = reader.read_waker.take() {
                waker.wake();
            }
            **guard = Err(error.clone());
        }
    }
}

// The reader for the application to read the received [datagram frames].
///
/// [datagram frames]: https://www.rfc-editor.org/rfc/rfc9221.html
#[derive(Debug, Clone)]
pub struct DatagramReader(Arc<Mutex<Result<RawDatagarmReader, Error>>>);

impl DatagramReader {
    /// Returns the number of new Datagrams dropped because the receive queue was full.
    pub fn dropped_queue_full(&self) -> io::Result<u64> {
        lock_unpoisoned(&self.0)
            .as_ref()
            .map(|reader| reader.dropped_queue_full)
            .map_err(|error| io::Error::from(error.clone()))
    }

    // Poll to receive a [datagram frame] from peer.
    ///
    /// This is the internal implementation of the [`DatagramReader::recv`] method.
    ///
    /// If the datagram is not ready, and the connection is active,
    /// the method will return [`Poll::Pending`] and set the waker for waking up the task when the datagram is received.
    ///
    /// Note that only the waker set by the last call may be awakened
    ///
    /// While there has a datagram frame received but unread,
    /// this method will return [`Poll::Ready`] with the received datagram frame as [`Ok`].
    ///
    /// If the connection is closing or already closed,
    /// this method will return [`Poll::Ready`] with an error as [`Err`].
    ///
    /// [datagram frame]: https://www.rfc-editor.org/rfc/rfc9221.html
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<io::Result<Bytes>> {
        let mut reader = lock_unpoisoned(&self.0);
        match reader.as_mut() {
            Ok(reader) => match reader.rcvd_datagrams.pop_front() {
                Some(bytes) => Poll::Ready(Ok(bytes)),
                None => {
                    reader.read_waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            },
            Err(e) => Poll::Ready(Err(io::Error::from(e.clone()))),
        }
    }

    /// Receive a [datagram frame] from peer.
    ///
    /// This method is asynchronous and returns a future that resolves to the received datagram.
    ///
    /// ``` rust, ignore
    /// pub async fn recv(&self) -> io::Result<Bytes>
    /// ```
    ///
    /// The future will yield the received datagram as [`Ok`].
    ///
    /// If the connection is closing or already closed, the future will yield an error as [`Err`].
    ///
    /// The future is *Cancel Safe*.
    ///
    /// [datagram frame]: https://www.rfc-editor.org/rfc/rfc9221.html
    pub fn recv(&mut self) -> RecvDatagram<'_> {
        RecvDatagram { reader: self }
    }

    /// Reads the received [datagram frame] into a mutable slice.
    ///
    /// This method is asynchronous and returns a future that resolves to the number of bytes read.
    ///
    /// ``` rust, ignore
    /// pub async fn read(&self, buf: & mut [u8]) -> io::Result<usize>
    /// ```
    ///
    /// The future will yield the size of bytes read from the received datagram as [`Ok`].
    ///
    /// If the buffer is not large enough to hold the received data, the received data will be truncated.
    ///
    /// If the connection is closing or already closed, the future will yield an error as [`Err`].
    ///
    /// [datagram frame]: https://www.rfc-editor.org/rfc/rfc9221.html
    pub fn read<'b>(&'b mut self, buf: &'b mut [u8]) -> ReadIntoSlice<'b> {
        ReadIntoSlice { reader: self, buf }
    }

    /// Reads the received [datagram frame] into a mutable reference to [`bytes::BufMut`].
    ///
    /// This method is asynchronous and returns a future that resolves to the number of bytes read.
    ///
    /// ``` rust, ignore
    /// pub async fn read_buf(&self, buf: & mut [u8]) -> io::Result<usize>
    /// ```
    ///
    /// The future will yield the size of bytes read from the received datagram as [`Ok`].
    ///
    /// If the buffer is not large enough to hold the received data, the behavior is defined by the [`bytes::BufMut::put`] implementation.
    ///
    /// If the connection is closing or already closed, the future will yield an error as [`Err`].
    ///
    /// [datagram frame]: https://www.rfc-editor.org/rfc/rfc9221.html
    pub fn read_buf<'b, B: BufMut>(&'b mut self, buf: &'b mut B) -> ReadIntoBuf<'b, B> {
        ReadIntoBuf { reader: self, buf }
    }
}

/// The [`Future`] created by [`DatagramReader::recv`], see [`DatagramReader::recv`] for more.
pub struct RecvDatagram<'a> {
    reader: &'a mut DatagramReader,
}

impl Future for RecvDatagram<'_> {
    type Output = io::Result<Bytes>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.reader.poll_recv(cx)
    }
}

/// the [`Future`] created by [`DatagramReader::read`], see [`DatagramReader::read`] for more.
pub struct ReadIntoSlice<'a> {
    reader: &'a mut DatagramReader,
    buf: &'a mut [u8],
}

impl Future for ReadIntoSlice<'_> {
    type Output = io::Result<usize>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let s = self.get_mut();
        let bytes = ready!(s.reader.poll_recv(cx)?);

        let len = bytes.len().min(s.buf.len());
        s.buf[..len].copy_from_slice(&bytes[..len]);
        Poll::Ready(Ok(len))
    }
}

/// the [`Future`] created by [`DatagramReader::read_buf`], see [`DatagramReader::read_buf`] for more.
pub struct ReadIntoBuf<'a, B> {
    reader: &'a mut DatagramReader,
    buf: &'a mut B,
}

impl<B> Future for ReadIntoBuf<'_, B>
where
    B: BufMut,
{
    type Output = io::Result<usize>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let s = self.get_mut();
        let bytes = ready!(s.reader.poll_recv(cx)?);

        let len = bytes.len();
        s.buf.put(bytes);
        Poll::Ready(Ok(len))
    }
}

#[cfg(test)]
mod tests {
    use qbase::{frame::FrameType, varint::VarInt};

    use super::*;

    #[tokio::test]
    async fn test_datagram_reader_recv_buf() {
        let incoming = DatagramIncoming::new(1024);

        let recv = tokio::spawn({
            let mut reader = incoming.new_reader().unwrap();
            async move {
                let n = reader.read(&mut [0u8; 1024]).await.unwrap();
                assert_eq!(n, 11);
            }
        });

        incoming
            .recv_datagram(
                &DatagramFrame::new(false, VarInt::from_u32(11)),
                Bytes::from_static(b"hello world"),
            )
            .unwrap();

        recv.await.unwrap();
    }

    #[tokio::test]
    async fn test_datagram_reader_on_conn_error() {
        let incoming = DatagramIncoming::new(1024);
        let error = QuicError::new(
            ErrorKind::ProtocolViolation,
            FrameType::Datagram(0).into(),
            "protocol violation",
        )
        .into();
        incoming.on_conn_error(&error);

        let new_reader = incoming.new_reader();
        assert!(new_reader.is_err());
        assert_eq!(new_reader.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn test_datagram_reader_drops_new_data_at_queue_capacity() {
        let incoming = DatagramIncoming::new(1024);
        let mut reader = incoming.new_reader().unwrap();
        let frame = DatagramFrame::new(true, VarInt::from_u32(1));

        for value in 0..DATAGRAM_QUEUE_CAPACITY {
            incoming
                .recv_datagram(&frame, Bytes::from(vec![value as u8]))
                .unwrap();
        }
        let two_byte_frame = DatagramFrame::new(true, VarInt::from_u32(2));
        incoming
            .recv_datagram(&two_byte_frame, Bytes::from_static(&[0xff, 0xff]))
            .unwrap();
        assert_eq!(reader.dropped_queue_full().unwrap(), 1);
        {
            let state = reader.0.lock().unwrap();
            let queued = state.as_ref().unwrap();
            assert_eq!(queued.rcvd_datagrams.len(), DATAGRAM_QUEUE_CAPACITY);
            assert_eq!(
                queued.rcvd_datagrams.back(),
                Some(&Bytes::from(vec![(DATAGRAM_QUEUE_CAPACITY - 1) as u8]))
            );
        }

        assert_eq!(reader.recv().await.unwrap(), Bytes::from_static(&[0]));
        incoming
            .recv_datagram(&two_byte_frame, Bytes::from_static(&[0xaa, 0xbb]))
            .unwrap();
        assert_eq!(reader.dropped_queue_full().unwrap(), 1);
        assert_eq!(
            reader
                .0
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .rcvd_datagrams
                .back(),
            Some(&Bytes::from_static(&[0xaa, 0xbb]))
        );
    }
}
