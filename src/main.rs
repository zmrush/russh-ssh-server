mod opsd;
mod sftp;
mod command;
mod async_fs_stream;
use log::debug;
use warp::Filter;
use std::io::Write;

fn with_tasks(tasks: crate::command::SafeTaskContainer)-> impl Filter<Extract = (crate::command::SafeTaskContainer,) ,Error = std::convert::Infallible> + Clone{
    warp::any().map(move || tasks.clone())
}

#[tokio::main]
pub async fn main() ->  std::result::Result<(), Box<dyn std::error::Error>>{
    let mut builder = env_logger::Builder::from_default_env();
    // builder.format_timestamp_micros();
    let builder = builder.format(|buf, record| {
        writeln!(
            buf,
            "[{} {} {}] [{}:{}] - {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S.3f %z"),
            record.level(),
            record.module_path().unwrap_or("unknown"),
            record.file().unwrap_or("unknown"),
            record.line().unwrap_or(0),
            record.args()
        )
    });

    builder
    .filter_level(log::LevelFilter::Info)
    .init();
    let home_dir =  std::env::var("HOME").unwrap();
    let client_pub = russh_keys::load_public_key(home_dir + "/.ssh/id_ed25519.pub").unwrap();
    // let client_pub = russh_keys::key::KeyPair::generate_ed25519().unwrap().clone_public_key().unwrap();
    debug!("client:{:?}",client_pub);

    let ops_ssh_route = warp::path!( "ops" / "ssh")
    .and(warp::ws())
    .map(|ws: warp::ws::Ws| {
        ws.on_upgrade(|websocket| crate::opsd::ssh_server(websocket))
    });
    let tasks = crate::command::SafeTaskContainer::new();
    let ops_run_command_route = warp::path!("ops" / "run_command")
    .and(warp::body::content_length_limit(1024 * 16))
    .and(warp::body::json())
    .and(with_tasks(tasks.clone()))
    .and_then(|req,tasks| async move{
        let resp = crate::command::run_command(req, tasks).await;
        if resp.is_ok(){
            let response = crate::command::RunCommandResponse{
                code: "Success".to_string(),
                message: None
               };
            Ok(Box::new(serde_json::to_string(&response).unwrap()))
        }else{
            let resp = &*resp.err().unwrap();
            if resp.is::<crate::command::CommandError>(){
                if let Some(e) = resp.downcast_ref::<crate::command::CommandError>(){
                    let response = crate::command::RunCommandResponse{
                        code: e.code.clone(),
                        message: Some(e.message.clone())
                       };
                    return Ok(Box::new(serde_json::to_string(&response).unwrap()));
                }
            }
            Err(warp::reject::reject())
        }
    } );
    let ops_describe_command_route = warp::path!("ops" / "describe_command")
    .and(warp::body::content_length_limit(1024 * 16))
    .and(warp::body::json())
    .and(with_tasks(tasks.clone()))
    .and_then(crate::command::describe_command);
    let ops_stop_command_route = warp::path!("ops" / "stop_command")
    .and(warp::body::content_length_limit(1024 * 16))
    .and(warp::body::json())
    .and(with_tasks(tasks.clone()))
    .and_then(crate::command::stop_command);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:7777").await.unwrap();
    let incoming =  tokio_stream::wrappers::TcpListenerStream::new(listener);
    warp::serve(ops_ssh_route.or(ops_run_command_route).or(ops_describe_command_route).or(ops_stop_command_route)).serve_incoming(incoming).await;

    //warp::serve(ops_ssh_route).run(([0,0,0,0],7777)).await;
    Ok(())
    // let (tcp_tx,tcp_rx) = tokio::sync::oneshot::channel();
    // let (_,server) = warp::serve(ops_ssh_route).bind_with_graceful_shutdown(([0, 0, 0, 0], 7777), async move {
    //     info!("rust tcp server started");
    //     tcp_rx.await.ok();
    //     info!("rust tcp servert receive close signal");
    // });
    // let t = tokio::spawn(server);
    // let signals = Signals::new(&[SIGTERM])?;
    // std::thread::spawn(move || {
    //     for _ in signals.forever() {
    //         let _ = tcp_tx.send(());
    //         info!("start to close server");
    //         break;
    //     }
    // });
    // t.await?;
    // Ok(())
}
