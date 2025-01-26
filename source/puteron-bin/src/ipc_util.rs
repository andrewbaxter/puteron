use {
    puteron::interface::ipc::{
        ipc,
        ipc_path,
    },
};

pub(crate) async fn client_req<I: ipc::ReqTrait>(req: I) -> Result<I::Resp, loga::Error> {
    return Ok(
        ipc::Client::new(ipc_path().unwrap()).await.map_err(loga::err)?.send_req(req).await.map_err(loga::err)?,
    );
}
