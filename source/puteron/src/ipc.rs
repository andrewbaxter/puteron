use {
    puteron_lib::interface::message::ipc,
    std::{
        env,
        path::PathBuf,
    },
};

pub(crate) fn ipc_path() -> Option<PathBuf> {
    if let Ok(p) = env::var("PUTERON_IPC_SOCK") {
        return Some(PathBuf::from(p));
    }
    if let Ok(p) = env::var("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(p).join("puteron.sock"));
    }
    return Some(PathBuf::from("/run/puteron.sock"));
}

pub(crate) async fn client_req<I: ipc::ReqTrait>(req: I) -> Result<I::Resp, loga::Error> {
    return Ok(
        ipc::Client::new(ipc_path().unwrap()).await.map_err(loga::err)?.send_req(req).await.map_err(loga::err)?,
    );
}
