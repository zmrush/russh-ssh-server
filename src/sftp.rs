use std::collections::HashMap;

use async_trait::async_trait;
use log::{error, info};

use russh_sftp::protocol::{ FileAttributes, Handle, Status, StatusCode, Version};
use tokio::io::{AsyncWriteExt, AsyncSeekExt, AsyncReadExt};
use russh_sftp::protocol::{OpenFlags,Data};
pub struct SftpSession {
    version: Option<u32>,
    fds: HashMap<String,tokio::fs::File>
}

impl Default for SftpSession {
    fn default() -> Self {
        Self {
            version: None,
            fds: HashMap::new()
        }
    }
}
pub struct SFTPError{
    inner: std::io::Error
}
impl From<std::io::Error> for SFTPError{
    fn from(value: std::io::Error) -> Self{
        Self{inner: value}
    }
}
impl Into<StatusCode> for SFTPError{
    fn into(self) -> StatusCode{
        let err_kind = self.inner.kind() as u32;
        (err_kind + 2).into()
    }
}

#[async_trait]
impl russh_sftp::server::Handler for SftpSession {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        version: u32,
        extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        if self.version.is_some() {
            error!("duplicate SSH_FXP_VERSION packet");
            return Err(StatusCode::ConnectionLost);
        }

        self.version = Some(version);
        info!("version: {:?}, extensions: {:?}", self.version, extensions);
        Ok(Version::new())
    }
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let mut open = tokio::fs::OpenOptions::new();
        open.create((pflags & OpenFlags::CREATE).bits() != 0).create_new((pflags & OpenFlags::EXCLUDE).bits() !=0);
        open.read((pflags & OpenFlags::READ).bits() != 0).write((pflags & OpenFlags::WRITE).bits() != 0);
        open.append((pflags & OpenFlags::APPEND).bits() != 0).truncate((pflags & OpenFlags::TRUNCATE).bits() != 0);
        open.mode(attrs.permissions.unwrap());
        let file = open.open(filename.as_str()).await;
        if file.is_err(){
            error!("open file error:{:?}",file.err());
            return Err(StatusCode::Failure);
        }
        let file = file.unwrap();
        self.fds.insert(filename.clone(), file);
        return Ok(Handle { id: id, handle: filename });
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        let _ = self.fds.remove(handle.as_str());
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        })
    }
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let file = self.fds.get_mut(handle.as_str());
        if file.is_none(){
            error!("read file for id:{},offset:{},handle:{},error:file not exist",id,offset,handle);
            return Err(StatusCode::NoSuchFile);
        }
        let file = file.unwrap();
        let meta = file.metadata().await;
        if meta.is_err(){
            error!("read file for id:{},offset:{},handle:{},error:{:?}",id,offset,handle,meta.err());
            return Err(StatusCode::Failure);
        }
        let meta = meta.unwrap();
        let meta_len = meta.len();
        if offset >= meta_len{
            return Err(StatusCode::Eof);
        }
        let res = file.seek(std::io::SeekFrom::Start(offset)).await;
        if res.is_err(){
            error!("read file for id:{},offset:{},handle:{},error:{:?}",id,offset,handle,res.err());
            return Err(StatusCode::Failure);
        }

        use bytes::BytesMut;
        let mut read_len = len;
        if len as u64 > meta_len - offset{
            read_len = (meta_len - offset) as u32;
        }
        let mut buffer = BytesMut::with_capacity(read_len as usize);
        file.read_buf(& mut buffer).await.map_err(|e| {
            let sc: StatusCode = SFTPError::from(e).into();
            sc
        })?;
        return Ok(Data { id: id, data: buffer.to_vec() });
    }

    /// Called on SSH_FXP_WRITE
    #[allow(unused_variables)]
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let file = self.fds.get_mut(handle.as_str());
        if file.is_none(){
            error!("write file for id:{},offset:{},handle:{},error:file not exist",id,offset,handle);
            return Err(StatusCode::NoSuchFile);
        }
        let file = file.unwrap();
        let res = file.write_all(&data).await;
        if res.is_err(){
            error!("write file for id:{},offset:{},handle:{},error:{:?}",id,offset,handle,res.err());
            return Err(StatusCode::Failure);
        }
        let res = file.flush().await;
        if res.is_err(){
            error!("write file for id:{},offset:{},handle:{},error:{:?}",id,offset,handle,res.err());
            return Err(StatusCode::Failure);
        }
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        })
    }
}