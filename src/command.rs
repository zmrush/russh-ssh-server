
use std::os::unix::process::CommandExt;

use serde::{Serialize, Deserialize};
use tokio::io::AsyncReadExt;
use log::info;
use std::os::unix::prelude::FromRawFd;

use crate::async_fs_stream::AsyncFsStream;

#[derive(Serialize, Deserialize)]
pub struct RunCommandRequest{
    pub command_id: String,
    pub command: Vec<u8>,
    pub timeout: u32,
    pub kill_mode: bool,
    pub client_token: String
}

#[derive(Serialize, Deserialize)]
pub struct RunCommandResponse{
    pub code: String,
    pub message: Option<String>
}
#[derive(Serialize, Deserialize)]
pub struct StopCommandRequest{
    pub command_id: String
}
#[derive(Serialize, Deserialize)]
pub struct StopCommandResponse{
    pub code: String,
    pub message: Option<String>
}
#[derive(Serialize, Deserialize)]
pub struct DescribeCommandRequest{
    pub command_id: String,
    pub output: bool
}
#[derive(Serialize, Deserialize)]
pub struct 
DescribeCommandResponse{
    pub code: String,
    pub message: Option<String>,
    pub status: Option<String>,
    pub output: Option<Vec<u8>>
}
#[derive(Clone)]
pub struct Task{
    pub command_id: String,
    pub status: String,
    pub output: Vec<u8>,
    pub client_token: String,
    pub sender: tokio::sync::mpsc::UnboundedSender<()>
}
#[derive(Clone)]
pub struct SafeTaskContainer{
    pub tasks: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String,Task>>>
}
impl SafeTaskContainer{
    pub fn new() -> Self{
        return Self{tasks: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()))};
    }
    pub async fn exist(&self,command_id: &str) -> bool{
        self.tasks.read().await.get(command_id).is_some()
    }
    pub async fn get_client_token(&self,command_id: &str) ->Option<String>{
        self.tasks.read().await.get(command_id).and_then(|task| Some(task.client_token.clone()))
    }
    pub async fn delete(&mut self,command_id: &str) {
        self.tasks.write().await.remove(command_id);
    }
    pub async fn insrt(&mut self,task: Task){
        self.tasks.write().await.insert(task.command_id.clone(), task);
    }
    pub async fn update(&mut self,command_id: &str,status: &str){
        self.tasks.write().await.get_mut(command_id).and_then(|task| {
            task.status = String::from(status);
            Some(())
        });
    }
    pub async fn update_output(&mut self,command_id: &str, output: Vec<u8>){
        self.tasks.write().await.get_mut(command_id).and_then(|task| {
            task.output = output;
            Some(())
        });
    }
    pub async fn get_status(&self,command_id: &str) -> Option<String>{
        self.tasks.read().await.get(command_id).and_then(|task| Some(String::from(task.status.as_str())))
    }
    pub async fn get_output(&self,command_id: &str) ->Option<Vec<u8>>{
        self.tasks.read().await.get(command_id).and_then(|task| Some(task.output.clone()))
    }
    pub async fn send_interrupt(&mut self,command_id: &str){
        self.tasks.write().await.get_mut(command_id).and_then(|task| {
            let res = task.sender.send(());
            if res.is_err(){
                info!("send interrupt err:{:?}",res.err());
            }
            Some(())
        });
    }
}
#[derive(Debug)]
pub struct CommandError{
    pub code: String,
    pub message: String
}
impl std::fmt::Display for CommandError {
     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
             write!(f, "command error:{}",self.message)
     }
}
impl std::error::Error for CommandError{
    fn description(&self) -> &str {
        return self.message.as_str();
    }
}
pub async fn run_command(request: RunCommandRequest,mut tasks: SafeTaskContainer) -> std::result::Result<(),Box<dyn std::error::Error>>{
    match tasks.get_client_token(&request.command_id).await{
        Some(client_token) =>{
            if client_token == request.client_token{
                return Ok(());
            }else{
                return Err(Box::new(CommandError{code: "Client.InvalidParam".to_string(),message: "client token not match with command ID".to_string()}));
            }
        }
        None =>{}
    }
    
    let (sender,mut recver) = tokio::sync::mpsc::unbounded_channel();
    
    let task = Task{
        command_id: request.command_id.clone(),
        status: "Running".to_string(),
        client_token: request.client_token,
        output: Vec::new(),
        sender: sender
    };
    tasks.insrt(task).await;
    let mut task_clone = tasks.clone();
    let command_id = request.command_id.clone();
    let (p_r,p_w) = nix::unistd::pipe()?;
    tokio::spawn(async move{
        let mut async_stream = AsyncFsStream::new(p_r,false).unwrap();
        let mut output = Vec::new();
        let _ = async_stream.read_to_end(&mut output).await;
        task_clone.update_output(command_id.as_str(), output).await;
    });
    tokio::spawn( async move {
        //开始执行命令
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(String::from_utf8_lossy(&request.command).into_owned());
        info!("command:{}",String::from_utf8_lossy(&request.command).into_owned());
        //非kill mode,意思是和父进程的pgid是不同的
        if !request.kill_mode{
            cmd.process_group(0);
        }
        cmd.stdout(unsafe { std::process::Stdio::from_raw_fd(p_w) });
        cmd.stderr(unsafe { std::process::Stdio::from_raw_fd(p_w) });
        let mut cmd = tokio::process::Command::from(cmd);
        let child = cmd.spawn();
        if child.is_err(){
            info!("start command error:{:?} for command_id:{},command:{:?}",child.err(),request.command_id,request.command);
            tasks.update(request.command_id.as_str(), "Failed").await;
            return;
        }
        let mut child = child.unwrap();
        let timer_exec_fut = tokio::time::timeout(std::time::Duration::from_secs(request.timeout as u64), child.wait());
        tokio::select! {
            time_exec = timer_exec_fut => {
                match time_exec{
                    Err(e) =>{
                        //将执行结果写入，超时执行
                        info!("timeout:{:?}", e);
                        let _ = child.kill().await;
                        tasks.update(&request.command_id, "Failed").await;
                    }
                    Ok(exec_status_res) =>{
                        match exec_status_res{
                            Err(e) =>{
                                info!("exec failed:{:?}",e);
                                tasks.update(&request.command_id, "Failed").await;
                                return;
                            }
                            Ok(exec_status) =>{
                                if !exec_status.success(){
                                    info!("exit failed2:{:?}",exec_status);
                                    tasks.update(&request.command_id, "Failed").await;
                                }
                                tasks.update(&request.command_id, "Success").await;
                            }
                        }
                    }
                }
            }
            Some(_) = recver.recv() =>{
                //有人停止
                let _ = child.kill().await;
                if let Some(status) = tasks.get_status(&request.command_id).await{
                    if status == "Running"{
                        tasks.update(&request.command_id, "Failed").await
                    }
                }
            }
        }
        
    });
    return Ok(());
}

pub async fn describe_command(request: DescribeCommandRequest,mut tasks: SafeTaskContainer) -> std::result::Result<Box<dyn warp::Reply>,warp::Rejection>{
    if !tasks.exist(&request.command_id).await{
        let response = DescribeCommandResponse{
            code: "Client.InvalidParam".to_string(),
            message: Some("the specified command doesn't exist".to_string()),
            status: None,
            output: None
        };
        return Ok(Box::new(serde_json::to_string(&response).unwrap()));
    }
    if let Some(status) = tasks.get_status(&request.command_id).await{
        if status == "Running"{
            let response = DescribeCommandResponse{
                code: "Success".to_string(),
                message: None,
                status: Some("Running".to_string()),
                output: None
            };
            return Ok(Box::new(serde_json::to_string(&response).unwrap()));
        }else{
            let vmm = if request.output{
                tasks.get_output(&request.command_id).await
            } else{
                None
            };
            let response = DescribeCommandResponse{
                code: "Success".to_string(),
                message: None,
                status: Some(status),
                output: vmm
            };
            tokio::spawn(async move{
                //完成状态的任务需在30分钟后自动清理
                info!("prepare to clean task:{}",request.command_id);
                tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
                tasks.delete(&request.command_id).await;
            });
            return Ok(Box::new(serde_json::to_string(&response).unwrap()));
        }
    }
    return Err(warp::reject());

}

pub async fn stop_command(request: StopCommandRequest,mut tasks: SafeTaskContainer) -> std::result::Result<Box<dyn warp::Reply>,warp::Rejection>{
    if !tasks.exist(&request.command_id).await{
        let response = StopCommandResponse{
            code: "Client.InvalidParam".to_string(),
            message: Some("the specified command doesn't exist".to_string()),
        };
        return Ok(Box::new(serde_json::to_string(&response).unwrap()));
    }
    if let Some(status) = tasks.get_status(&request.command_id).await{
        let mut task_clone = tasks.clone();
        let command_id = request.command_id.clone();
        tokio::spawn(async move{
            //停止任务后我们需要在30分钟内清理掉这个任务
            info!("prepare to clean task:{}",command_id);
            tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
            task_clone.delete(command_id.as_str()).await;
        });
        if status != "Running"{
            let response = StopCommandResponse{
                code: "Success".to_string(),
                message: Some("the specified command has exited".to_string()),
            };
            return Ok(Box::new(serde_json::to_string(&response).unwrap()));
        }
        if status == "Running"{
            tasks.update(&request.command_id, "Interrupt").await;
        }
        let _ = tasks.send_interrupt(&request.command_id).await;
        let response = StopCommandResponse{
            code: "Success".to_string(),
            message: None,
        };
        return Ok(Box::new(serde_json::to_string(&response).unwrap()));
    }
    return Err(warp::reject());
}