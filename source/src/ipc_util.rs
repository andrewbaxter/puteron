use {
    crate::interface::ipc::{
        ipc,
        ipc_path,
    },
};

pub async fn client() -> Result<ipc::Client, loga::Error> {
    return Ok(ipc::Client::new(ipc_path().unwrap()).await.map_err(loga::err)?);
}

pub async fn client_req<I: ipc::ReqTrait>(req: I) -> Result<I::Resp, loga::Error> {
    return Ok(client().await?.send_req(req).await.map_err(loga::err)?);
}
