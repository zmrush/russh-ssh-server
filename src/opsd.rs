use async_trait::async_trait;

use russh::server::{Msg, Session};
use russh::*;
use russh_keys::*;
use tokio::sync::Mutex;
use nix::{pty::openpty, unistd::setsid};
use tokio::process::Command;
use std::{os::unix::prelude::FromRawFd, process::Stdio};
use log::{debug, warn, info};
use std::sync::Arc;
use std::collections::HashMap;
use warp::ws::WebSocket;
use russh::server::run_stream;
use tokio::io::{AsyncRead,AsyncWrite, ReadBuf};
use bytebuffer::ByteBuffer;
use futures_util::{Stream,Sink};
use crate::async_fs_stream::AsyncFsStream;
use std::pin::Pin;
use std::task::{Context, Poll};
use futures::ready;
use std::os::fd::RawFd;
// use pty_process::Pty;

pub struct ExtractWebsocketStream{
    pub websocket: WebSocket,
    pub byte_buffer: ByteBuffer
}

impl AsyncRead for ExtractWebsocketStream{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>>{
        if !self.byte_buffer.is_empty(){
            let next_len = buf.remaining().min(self.byte_buffer.len()-self.byte_buffer.get_rpos());
            let rrr = self.byte_buffer.read_bytes(next_len).unwrap();
            if self.byte_buffer.get_rpos() == self.byte_buffer.len(){
                self.byte_buffer.clear();
            }
            buf.put_slice(&rrr);
            return Poll::Ready(Ok(()));
        }
        match ready!(Pin::new(&mut self.websocket).poll_next(cx)) {
            Some(Ok(item)) => {
                if item.is_binary(){
                    let bin = item.as_bytes();
                    if buf.remaining()<bin.len(){
                        let now_read = buf.remaining().min(bin.len());
                        buf.put_slice(&bin[..now_read]);
                        self.byte_buffer.write_bytes(&bin[now_read..]);
                    }else{
                        buf.put_slice(&bin);
                    }
                    return Poll::Ready(Ok(()));
                }
                return Poll::Pending;
            }
            Some(Err(e)) => {
                info!("websocket poll error: {}", e);
                Poll::Ready(Err(std::io::Error::last_os_error()))
            }
            None => {
                info!("websocket closed");
                Poll::Ready(Ok(()))
            }
        }
    }
}
impl AsyncWrite for ExtractWebsocketStream{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, std::io::Error>>{
        match ready!(Pin::new(&mut self.websocket).poll_ready(cx)){
            Ok(()) => {
                let len = buf.len();
                let msg = warp::ws::Message::binary(buf);
                match Pin::new(&mut self.websocket).start_send(msg){
                    Ok(()) =>{
                        //因为russh本身的server session 缺乏自动的flush，因此我们需要强制的在start send后调用poll flush
                        // return Poll::Ready(Ok(len));
                        let flush_res = Pin::new(&mut self.websocket).poll_flush(cx);
                        match flush_res{
                            Poll::Pending =>{
                                //即使是pending,那就等待下一次去刷新
                                return Poll::Ready(Ok(len))
                            }
                            Poll::Ready(res) =>{
                                match res{
                                    Ok(())=>{
                                        return Poll::Ready(Ok(len))
                                    }
                                    Err(e) =>{
                                        info!("flush fail:{:?}",e);
                                        return Poll::Ready(Err(std::io::Error::last_os_error()))
                                    }
                                }
                            }
                        }
                    }
                    Err(e) =>{
                        info!("send fail:{:?}",e);
                        return Poll::Ready(Err(std::io::Error::last_os_error()))
                    }
                } 
                
            }
            Err(e) => {
                info!("write ready error:{:?}",e);
                Poll::Ready(Err(std::io::Error::last_os_error()))
            }
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::result::Result<(), std::io::Error>>{
        info!("poll flush");
        match ready!(Pin::new(&mut self.websocket).poll_flush(cx)){
            Ok(()) =>{
                Poll::Ready(Ok(()))
            }
            Err(e) =>{
                info!("flush fail:{:?}",e);
                Poll::Ready(Err(std::io::Error::last_os_error()))
            }
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::result::Result<(), std::io::Error>>{
        match ready!(Pin::new(&mut self.websocket).poll_close(cx)){
            Ok(()) =>{
                Poll::Ready(Ok(()))
            }
            Err(e) =>{
                info!("shutdown error:{:?}",e);
                Poll::Ready(Err(std::io::Error::last_os_error()))
            }
        }
    }
}

pub async fn ssh_server(websocket: WebSocket){
    let config = russh::server::Config {
        inactivity_timeout: Some(std::time::Duration::from_secs(3600)),
        auth_rejection_time: std::time::Duration::from_secs(3),
        auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
        keys: vec![russh_keys::key::KeyPair::generate_ed25519().unwrap()],
        ..Default::default()
    };
    let config = Arc::new(config);
    let client_pub = russh_keys::load_public_key("/Users/zhuming/.ssh/id_ed25519.pub").unwrap();
    let sh = Server {
        clients: Arc::new(Mutex::new(HashMap::new())),
        auth_client_key: client_pub,
        pty: None
    };

    let websocket_stream = ExtractWebsocketStream{websocket: websocket,byte_buffer: ByteBuffer::new()};
    let rs = run_stream(config, websocket_stream, sh).await.unwrap();
    let res = rs.await;
    if res.is_err(){
        info!("ssh server run encounter error:{:?}",res.err());
    }
    info!("end to run stream");
}

// #[derive(Clone)]
pub struct Server {
    pub clients: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    pub auth_client_key: russh_keys::key::PublicKey,
    // pub pty_slave: RawFd
    pub pty: Option<RawFd>
}

impl Server {
    pub async fn get_channel(&mut self, channel_id: ChannelId) -> Channel<Msg> {
        let mut clients = self.clients.lock().await;
        clients.remove(&channel_id).unwrap()
    }
}

// impl server::Server for Server {
//     type Handler = Self;
//     fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
//         let s = self.clone();
//         s
//     }
// }

#[async_trait]
impl server::Handler for Server {
    type Error = anyhow::Error;

    async fn channel_open_session(
        self,
        channel: Channel<Msg>,
        session: Session,
    ) -> std::result::Result<(Self, bool, Session), Self::Error> {
        {
            let mut clients = self.clients.lock().await;
            clients.insert(channel.id(), channel);
        }
        Ok((self, true, session))
    }
    async fn auth_publickey_offered(
        self,
        _: &str,
        public_key: &key::PublicKey,
    ) -> std::result::Result<(Self, server::Auth), Self::Error> {
        let compare = self.auth_client_key.eq(public_key);
        if compare{
            info!("public key is authorized");
            return Ok((
                self,
                server::Auth::Accept,
            ));
        }else{
            warn!("public key is not authorized");
            return Ok((
                self,
                server::Auth::Reject { proceed_with_methods: None }
            ));
        }
        
    }

    async fn auth_publickey(
        self,
        _: &str,
        _: &key::PublicKey,
    ) -> std::result::Result<(Self, server::Auth), Self::Error> {
        Ok((self, server::Auth::Accept))
    }

    async fn data(
        mut self,
        _: ChannelId,
        data: &[u8],
        session: Session,
    ) -> std::result::Result<(Self, Session), Self::Error> {
        info!("receive data:{:?}",data);
        Ok((self, session))
    }
    async fn channel_eof(
        self,
        _: ChannelId,
        session: Session,
    ) -> std::result::Result<(Self, Session), Self::Error> {
        Ok((self, session))
    }

    async fn subsystem_request(
        mut self,
        channel_id: ChannelId,
        name: &str,
        mut session: Session,
    ) -> std::result::Result<(Self, Session), Self::Error> {
        info!("subsystem: {}", name);

        if name == "sftp" {
            let channel = self.get_channel(channel_id).await;
            use crate::sftp::SftpSession;
            let sftp = SftpSession::default();
            session.channel_success(channel_id);
            russh_sftp::server::run(channel.into_stream(), sftp).await;
        } else {
            session.channel_failure(channel_id);
        }

        Ok((self, session))
    }

    async fn window_change_request(
        self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        session: Session,
    ) -> std::result::Result<(Self, Session), Self::Error> {
        info!("resize,col:{},row:{},p_w:{},p_h:{}",col_width,row_height,pix_width,pix_height);
        let size = libc::winsize {
            ws_row: row_height as u16,
            ws_col: col_width as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // let mut size = pty_process::Size::new(row_height as u16,col_width as u16);
        let ret = unsafe { 
            libc::ioctl(*self.pty.as_ref().unwrap(), libc::TIOCSWINSZ.into(),std::ptr::addr_of!(size))
        };
        // if ret.is_err(){
        //     info!("set winsize err:{:?}",ret.err());
        // }
        if ret == -1{
            let err = std::io::Error::last_os_error();
            info!("set winsize res:{},err:{:?}",ret,err);
        }
        
        
        Ok((self, session))
    }

    async fn pty_request(
        mut self,
        channel_id: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        modes: &[(Pty, u32)],
        session: Session,
    ) -> std::result::Result<(Self, Session), Self::Error>  {
        info!("term:{:?},modes:{:?}",term,modes);
        use libc::winsize;
        let winsize =  winsize{
            ws_col: col_width as u16,
            ws_row: row_height as u16,
            ws_xpixel: pix_width as u16,
            ws_ypixel: pix_height as u16
        };
        let pty = openpty(Some(&winsize), None).unwrap();
        let pty_master = pty.master;
        let mut cmd = Command::new("/bin/bash");
        cmd
        .env("PATH", "/usr/local/sbin:/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin:/root/bin")
        .env("LANG","en_US.UTF-8")
        .env("HISTSIZE","1000")
        .env("PWD","/Users/zhuming")
        .env("USER","zhuming")
        .env("HOME","/Users/zhuming")
        .env("TERM",term);
        cmd.stdin(unsafe { Stdio::from_raw_fd(pty.slave) });
        cmd.stdout(unsafe { Stdio::from_raw_fd(pty.slave) });
        cmd.stderr(unsafe { Stdio::from_raw_fd(pty.slave) });
        unsafe {
            cmd.pre_exec(move || {
                setsid().unwrap();
                Ok(())
            })
        };
        let _ = cmd.spawn();
        let channel = self.get_channel(channel_id.clone()).await;
        let stream = channel.into_stream();
        let (mut stream_reader, mut stream_writer) = tokio::io::split(stream);
        self.pty = Some(pty.master);
        let pty_fd = AsyncFsStream::new(pty_master,false).unwrap();
        tokio::spawn(async move {
            let (mut pty_reader, mut pty_writer) = tokio::io::split(pty_fd);
            tokio::select! {
                res = tokio::io::copy(&mut pty_reader, &mut stream_writer) => {
                    debug!("pty closed: {:?}", res);
                }
                res = tokio::io::copy(&mut stream_reader, &mut pty_writer) => {
                    debug!("stream closed: {:?}", res);
                }
            }
            use tokio::io::AsyncWriteExt;
            let _ = stream_writer.shutdown().await;
        });

        Ok((self, session))
    }
}