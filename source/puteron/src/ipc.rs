use {
    loga::{
        ErrContext,
        ResultContext,
    },
    serde::de::DeserializeOwned,
    std::{
        env,
        io::ErrorKind,
        path::PathBuf,
    },
    tokio::{
        io::{
            AsyncReadExt,
            AsyncWriteExt,
        },
        net::{
            UnixSocket,
            UnixStream,
        },
    },
};

pub(crate) fn ipc_path() -> Option<PathBuf> {
    if let Ok(p) = env::var("PUTERIUM_IPC_SOCK") {
        return Some(PathBuf::from(p));
    }
    return Some(PathBuf::from("/run/puteron.sock"));
}

pub(crate) async fn write(conn: &mut UnixStream, message: &[u8]) -> Result<(), loga::Error> {
    conn.write_u64_le(message.len() as u64).await.context("Error writing frame size")?;
    conn.write_all(message).await.context("Error writing frame body")?;
    return Ok(());
}

pub(crate) async fn read<O: DeserializeOwned>(conn: &mut UnixStream) -> Result<Option<O>, loga::Error> {
    let len = match conn.read_u64_le().await {
        Ok(len) => len,
        Err(e) => {
            if e.kind() == ErrorKind::BrokenPipe {
                return Ok(None);
            }
            return Err(e.context("Error reading message length from connection"));
        },
    };
    let mut body = vec![];
    body.reserve(len as usize);
    match conn.read_buf(&mut body).await {
        Ok(_) => { },
        Err(e) => {
            return Err(e.context("Error reading message body from connection"));
        },
    }
    return Ok(Some(serde_json::from_slice::<O>(&body).context("Error parsing message JSON")?));
}

pub(crate) async fn client_req<
    I: puteron_lib::interface::message::v1::RequestTrait,
>(req: I) -> Result<I::Response, loga::Error> {
    let mut conn =
        UnixSocket::new_stream()?
            .connect(ipc_path().context("No IPC path set; there is no default IPC path for non-root users")?)
            .await?;
    write(&mut conn, &serde_json::to_vec(&req).unwrap()).await?;
    return Ok(read::<I::Response>(&mut conn).await?.context("Disconnected by remote host")?);
}
