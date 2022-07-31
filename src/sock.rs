#![cfg(windows)]

use std::io::{Read, Write};
use std::task::Poll;

use std::path::Path;

use uds_windows::UnixStream;

use tokio::io;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// this is a fake async wrapper for uds_windows::UnixStream
// and poll_xxx functions will hang before the real operation fails
// TODO: consider implementing an async version in the future
pub struct WinUnixStream {
    stream: UnixStream,
}

impl WinUnixStream {
    pub async fn connect<P>(path: P) -> io::Result<WinUnixStream>
    where
        P: AsRef<Path>,
    {
        let stream = UnixStream::connect(path)?;
        Ok(WinUnixStream { stream })
    }
}

impl AsyncRead for WinUnixStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let b =
            unsafe { &mut *(buf.unfilled_mut() as *mut [std::mem::MaybeUninit<u8>] as *mut [u8]) };
        let n = self.stream.read(b)?;
        unsafe { buf.assume_init(n) };
        buf.advance(n);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for WinUnixStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        let size = self.stream.write(&buf)?;
        Poll::Ready(Ok(size))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.stream.flush()?;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.stream.shutdown(std::net::Shutdown::Both)?;
        Poll::Ready(Ok(()))
    }
}
