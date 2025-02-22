use {
    aargvark::{
        vark,
        Aargvark,
    },
    flowcontrol::ta_return,
    loga::{
        ea,
        fatal,
        DebugDisplay,
        ErrContext,
        ResultContext,
    },
    puteron::run_dir,
    rustix::{
        process::{
            pidfd_open,
            PidfdFlags,
        },
        termios::Pid,
    },
    std::{
        fs::remove_file,
        os::fd::OwnedFd,
        path::PathBuf,
        time::Duration,
    },
    tokio::{
        io::unix::{
            AsyncFd,
            AsyncFdReadyGuard,
        },
        process::Command,
        select,
        signal::unix::SignalKind,
        time::sleep,
    },
};

async fn systemctl_show_prop(unit: &str, prop: &str) -> Result<String, loga::Error> {
    let proc =
        Command::new("systemctl")
            .arg("show")
            .arg("--property")
            .arg(prop)
            .arg("--value")
            .arg(unit)
            .output()
            .await?;
    if !proc.status.success() {
        return Err(loga::err(format!("Error querying unit [{}] property [{}]", unit, prop)));
    }
    return Ok(
        String::from_utf8(proc.stdout.clone())
            .context_with(
                format!("Found unit [{}] property [{}] value is not valid utf-8", unit, prop),
                ea!(value = String::from_utf8_lossy(&proc.stdout)),
            )?
            .trim_ascii()
            .to_string(),
    );
}

/// This command effectively foregrounds control of a systemd unit. When this
/// command runs, it will start the unit. When it is killed (sigint, sigterm) it
/// will stop the unit.
///
/// You can use this to inject systemd control into a puteron graph.
///
/// If the unit has no MainPID after starting, doesn't wait for the process to exit
/// (i.e. control is one-directional).
///
/// This creates a file containing the unit PID (or empty if there's no associated
/// process) in the `/run` or `XDG_RUNTIME_DIR` directory with the name
/// `puteron-control-systemd-${unit}.pid`. When the unit stops, the file is deleted.
#[derive(Aargvark)]
struct Args {
    /// The unit to foreground
    unit: String,
}

struct PidFile {
    path: PathBuf,
}

impl PidFile {
    async fn new(path: PathBuf, pid: Option<Pid>) -> Self {
        let contents;
        if let Some(pid) = pid {
            contents = pid.as_raw_nonzero().to_string();
        } else {
            contents = "".to_string();
        }
        if let Err(e) = tokio::fs::write(&path, contents).await {
            eprintln!("Warning: failed to create pid file at [{:?}]: {}", path, e);
        }
        return Self { path: path };
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if let Err(e) = remove_file(&self.path) {
            eprintln!("Warning: Failed to delete pid file at [{:?}]: {}", self.path, e);
        }
    }
}

async fn main1() -> Result<(), loga::Error> {
    let args = vark::<Args>();
    let mut errors = vec![];
    match async {
        let mut c = Command::new("systemctl");
        c.arg("reset-failed").arg(&args.unit);
        let res =
            c
                .output()
                .await
                .context_with("Error attempting to reset unit status before launching", ea!(command = c.dbg_str()))?;
        if !res.status.success() {
            return Err(
                loga::err_with(
                    "Failed to reset unit status before launching",
                    ea!(command = c.dbg_str(), output = res.dbg_str()),
                ),
            );
        }
        return Ok(());
    }.await {
        Ok(_) => { },
        Err(e) => {
            eprintln!("Warning: Failed resetting unit status: {}", e);
        },
    }
    let pidfile_path = run_dir().join(format!("puteron-control-systemd-{}.pid", args.unit));
    match Command::new("systemctl").arg("start").arg(&args.unit).output().await {
        Ok(_) => {
            match async {
                let mut cleanup_pid = None;
                let wait_work = async {
                    loop {
                        match async {
                            ta_return!((), loga::Error);
                            if systemctl_show_prop(&args.unit, "SubState")
                                .await
                                .context("Error getting unit result")? !=
                                "running" {
                                return Ok(());
                            }
                            let pid =
                                systemctl_show_prop(&args.unit, "MainPID")
                                    .await
                                    .context("Error querying unit PID")?;
                            if pid.is_empty() {
                                // No PID expected?.
                                cleanup_pid = Some(PidFile::new(pidfile_path.clone(), None).await);
                                std::future::pending::<()>().await;
                                unreachable!();
                            };
                            let pid =
                                i32::from_str_radix(
                                    &pid.trim_ascii(),
                                    10,
                                ).context_with("Found PID is not a valid i32", ea!(pid = pid))?;
                            if pid == 0 {
                                // Still starting up?
                                return Err(loga::err("PID is 0, still starting?"));
                            } else {
                                let pid =
                                    Pid::from_raw(
                                        pid,
                                    ).context_with("Found PID is not a valid PID (not positive)", ea!(pid = pid))?;
                                let pidfd =
                                    pidfd_open(
                                        pid,
                                        PidfdFlags::NONBLOCK,
                                    ).context_with("Error opening pidfd for unit", ea!(pid = pid.dbg_str()))?;
                                let pidfd =
                                    AsyncFd::new(
                                        pidfd,
                                    ).context_with(
                                        "Error converting pidfd to asyncfd for unit",
                                        ea!(pid = pid.dbg_str()),
                                    )?;
                                cleanup_pid = Some(PidFile::new(pidfile_path.clone(), None).await);
                                let _: AsyncFdReadyGuard<OwnedFd> = match pidfd.readable().await {
                                    Ok(x) => x,
                                    Err(e) => {
                                        eprintln!(
                                            "Error waiting for pidfd for pid {:?} to report readable: {}",
                                            pid,
                                            e
                                        );
                                        return Ok(());
                                    },
                                };
                                return Ok(());
                            }
                        }.await {
                            Ok(_) => {
                                return;
                            },
                            Err(e) => {
                                eprintln!("Error waiting for unit, retrying: {}", e);
                            },
                        }
                        sleep(Duration::from_secs(1)).await;
                    }
                };
                let mut sigint =
                    tokio::signal::unix::signal(SignalKind::interrupt()).context("Error hooking into SIGINT")?;
                let sigint = Box::pin(sigint.recv());
                let mut sigterm =
                    tokio::signal::unix::signal(SignalKind::terminate()).context("Error hooking into SIGTERM")?;
                let sigterm = Box::pin(sigterm.recv());
                select!{
                    _ = wait_work => {
                        let result =
                            systemctl_show_prop(&args.unit, "Result").await.context("Error getting unit result")?;
                        if result != "success" {
                            return Err(loga::err(format!("Unit exited with non-success result: {}", result)));
                        }
                    },
                    _ = sigint => {
                    },
                    _ = sigterm => {
                    }
                };
                return Ok(());
            }.await {
                Ok(_) => { },
                Err(e) => {
                    errors.push(e);
                },
            }
        },
        Err(e) => {
            errors.push(e.context("Error starting unit"));
        },
    }
    if let Err(e) = Command::new("systemctl").arg("stop").arg(&args.unit).output().await {
        errors.push(e.context("Error stopping unit"));
    }
    if !errors.is_empty() {
        return Err(loga::agg_err("Encountered one or more errors", errors));
    }
    return Ok(());
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = main1().await {
        fatal(e);
    }
}
