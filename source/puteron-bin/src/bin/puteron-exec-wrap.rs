//! Launched processes must be in their own process group in order to handle
//! signals independently.
//!
//! Launched processes must be in their own session in order to switch vts and
//! stuff.
//!
//! Setting the process group id prevents setting a new session, so instead of
//! launching in a new process group launch in the same process group, then call
//! `setsid` to get a new process group and session.
//!
//! Ideally with this launched processes should be able to do anything
//! (crossing-fingers x999).
use {
    aargvark::{
        vark,
        Aargvark,
    },
    libc::{
        execv,
        strerror,
    },
    rustix::process::{
        getpgid,
        getpid,
        setsid,
    },
    std::{
        ffi::{
            CStr,
            CString,
        },
        ptr::null,
    },
};

#[derive(Aargvark)]
struct Args {
    args: Vec<String>,
}

fn main() {
    let args = vark::<Args>();
    {
        let pgid = getpgid(None).expect("Failed to get current pgid");
        let pid = getpid();
        if pgid == pid {
            eprintln!("Launched as process group leader (pgid {:?} vs pid {:?}) -- setsid will fail", pgid, pid);
        }
    }
    setsid().expect("Setsid failed");
    let prog = CString::new(args.args[0].clone()).unwrap();
    let mut argv0 = vec![];
    let mut argv = vec![];
    for arg in args.args {
        argv0.push(CString::new(arg).unwrap());
    }
    for arg in &argv0 {
        argv.push(arg.as_ptr());
    }
    argv.push(null());
    unsafe {
        let res = execv(prog.as_ptr(), argv.as_ptr());
        if res != 0 {
            panic!("Execve failed with exit {}: {}", res, CStr::from_ptr(strerror(res)).to_string_lossy());
        }
    }
}
