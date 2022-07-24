use std::path::Path;
use uds_windows;

use tokio::io;
use tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf, Ready};

pub struct UnixStream {
    stream: uds_windows::UnixStream,
}

impl UnixStream {
    pub async fn connect<P>(path: P) -> io::Result<UnixStream>
    where
        P: AsRef<Path>,
    {
        let stream = uds_windows::UnixStream::new(path)?;
        Ok(UnixStream { stream })
    }

    pub async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        Ok(Ready::READABLE)
    }
    
    fn write<'a>(&'a mut self, src: &'a [u8]) -> Write<'a, Self>
    where
        Self: Unpin,
    {
        self.stream.write(src)
    }
}
