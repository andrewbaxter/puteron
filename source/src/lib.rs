pub mod interface;
pub mod time;
pub mod demon;
pub mod ipc_util;
pub mod spec;

use {
    std::{
        env,
        path::PathBuf,
    },
};

pub fn run_dir() -> PathBuf {
    if let Ok(p) = env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(p);
    }
    return PathBuf::from("/run");
}
