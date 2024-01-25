use tokio::io::unix::AsyncFd;
use std::os::fd::{RawFd, FromRawFd};
use std::{fs::File, os::fd::AsRawFd};
use std::io::{Result, Write};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use futures::ready;
use log::info;
use nix::{
    errno::Errno,
    unistd::{read, write},
};
pub struct AsyncFsStream{
    fd: AsyncFd<RawFd>,
    file: Option<File>
}
impl AsyncFsStream{
    pub fn new(fd: RawFd,fd_is_file: bool) -> Result<Self>{
        unsafe{
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        if fd_is_file{
            unsafe{
                Ok(Self{ fd:AsyncFd::new(fd)?,file: Some(File::from_raw_fd(fd))})
            }
        }else{
            Ok(Self{fd:AsyncFd::new(fd)?,file: None})
        }
    }
    #[allow(dead_code)]
    pub async fn read(&self, out: &mut [u8]) -> Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;

            match guard.try_io(|inner| read(*inner.get_ref(),out).map_err(Errno::into)) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }
    #[allow(dead_code)]
    pub async fn write(&self, buf: &[u8]) -> Result<usize> {
        loop {
            let mut guard = self.fd.writable().await?;

            match guard.try_io(|inner| write(*inner.get_ref(), buf).map_err(Errno::into)) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }
}
impl AsyncRead for AsyncFsStream{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>>{
        loop{
            //let mut guard = ready!(self.fd.poll_read_ready(cx));
            let mut guard = ready!(self.fd.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            match guard.try_io(|inner| read(*inner.get_ref(),unfilled).map_err(Errno::into)) {
                Ok(Ok(len)) => {
                    buf.advance(len);
                    return Poll::Ready(Ok(()));
                },
                Ok(Err(err)) => {
                    info!("err:{:?}",err);
                    return Poll::Ready(Err(err))
                },
                Err(_would_block) => continue,
            }
        }
    }
}
impl AsyncWrite for AsyncFsStream{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8]
    ) -> Poll<Result<usize>> {
        loop {
            //let mut guard = ready!(self.fd.poll_write_ready(cx))?;
            let mut guard = ready!(self.fd.poll_write_ready(cx))?;
            match guard.try_io(|inner| write(*inner.get_ref(),buf).map_err(Errno::into)) {
                Ok(result) => {
                    return Poll::Ready(result)
                },
                Err(_would_block) => continue,
            }
        }
    }
    fn poll_flush(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<()>> {
        let file = self.file.as_ref();
        match file{
            None => Poll::Ready(Ok(())),
            Some( mut x) =>  Poll::Ready(x.flush())
        }
        // Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<()>> {
        //auto shutdown
        Poll::Ready(Ok(()))
    }

}