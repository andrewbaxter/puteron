use {
    crate::interface::ipc::{
        ipc,
        ipc_path,
    },
    std::{
        env,
        path::PathBuf,
    },
};

pub async fn client() -> Result<ipc::Client, loga::Error> {
    return Ok(ipc::Client::new(ipc_path().unwrap()).await.map_err(loga::err)?);
}

pub async fn client_req<I: ipc::ReqTrait>(req: I) -> Result<I::Resp, loga::Error> {
    return Ok(client().await?.send_req(req).await.map_err(loga::err)?);
}

pub fn run_dir() -> PathBuf {
    if let Ok(p) = env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(p);
    }
    return PathBuf::from("/run");
}
