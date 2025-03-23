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
/// command runs, it will start the unit. If the unit has a process, this waits
/// until the process exits or sigint/sigterm is received. When this command exits
/// it stops the unit.
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

struct StartedFile {
    path: PathBuf,
}

impl StartedFile {
    async fn new(path: PathBuf) -> Self {
        if let Err(e) = tokio::fs::write(&path, "").await {
            eprintln!("Warning: failed to create `started` file at [{:?}]: {}", path, e);
        }
        return Self { path: path };
    }
}

impl Drop for StartedFile {
    fn drop(&mut self) {
        if let Err(e) = remove_file(&self.path) {
            eprintln!("Warning: Failed to delete `started` file at [{:?}]: {}", self.path, e);
        }
    }
}

struct UnitMeta {
    /// True if the unit should be considered started once it has a PID, rather than
    /// once it exits (i.e. oneshot vs simple).
    started_at_pid: bool,
    /// True if the unit will have a pid while starting/running
    pid_relevant: bool,
    /// True if the systemd unit needs to be actively stopped (i.e. a mount, or a
    /// oneshot with remainafterexit) - so this unit should stay running to catch stop
    /// signals and translate them to systemd control.
    active_stop: bool,
}

async fn run_unit(unit: &str, unit_meta: UnitMeta) -> Result<(), loga::Error> {
    // Start
    if let Err(e) = Command::new("systemctl").arg("start").arg(unit).output().await {
        return Err(e.context("Error starting unit"));
    }

    // If has a pid, wait until the process exits
    let mut cleanup_started_file = None;
    let startedfile_path = run_dir().join(format!("puteron-control-systemd-{}-started", unit));
    loop {
        match async {
            ta_return!(bool, loga::Error);

            // Exit condition - done
            if systemctl_show_prop(unit, "SubState").await.context("Error getting unit result")? != "running" {
                return Ok(true);
            }

            // This part could be done by polling SubState, but wait on the pid to let this
            // sleep more. This section could be deleted and it should still work fine.
            //
            // Get the pid
            if unit_meta.pid_relevant {
                let pid = systemctl_show_prop(unit, "MainPID").await.context("Error querying unit PID")?;
                let pid =
                    i32::from_str_radix(
                        &pid.trim_ascii(),
                        10,
                    ).context_with("Found PID is not a valid i32", ea!(pid = pid))?;
                if pid == 0 {
                    // Still starting up?
                    return Err(loga::err("PID is 0, still starting?"));
                } else {
                    // Wait for pid
                    let pid = Pid::from_raw(pid).context_with("Found PID is not a valid PID (not positive)", ea!(pid = pid))?;
                    let pidfd =
                        pidfd_open(
                            pid,
                            PidfdFlags::NONBLOCK,
                        ).context_with("Error opening pidfd for unit", ea!(pid = pid.dbg_str()))?;
                    let pidfd =
                        AsyncFd::new(
                            pidfd,
                        ).context_with("Error converting pidfd to asyncfd for unit", ea!(pid = pid.dbg_str()))?;
                    if unit_meta.started_at_pid {
                        cleanup_started_file = Some(StartedFile::new(startedfile_path.clone()).await);
                    }
                    let _: AsyncFdReadyGuard<OwnedFd> = match pidfd.readable().await {
                        Ok(x) => x,
                        Err(e) => {
                            eprintln!("Error waiting for pidfd for pid {:?} to report readable: {}", pid, e);
                            return Ok(true);
                        },
                    };
                }
            }

            // Loop, poll
            return Ok(false);
        }.await {
            Ok(done) => {
                if done {
                    break;
                }
            },
            Err(e) => {
                eprintln!("Error waiting for unit to have PID, retrying: {}", e);
            },
        }
        sleep(Duration::from_secs(1)).await;
    }

    // Check result
    let result = systemctl_show_prop(unit, "Result").await.context("Error getting unit result")?;
    match result.as_ref() {
        "success" | "mounted" => { },
        result => {
            return Err(loga::err(format!("Unit exited with non-success result: {}", result)));
        },
    }

    // For units that "complete" but are perpetual, stay running to catch puteron
    // "stop"
    if unit_meta.active_stop {
        if cleanup_started_file.is_none() {
            cleanup_started_file = Some(StartedFile::new(startedfile_path.clone()).await);
        }
        std::future::pending::<()>().await;
    }
    _ = cleanup_started_file;
    return Ok(());
}

async fn main1() -> Result<(), loga::Error> {
    let args = vark::<Args>();
    let mut errors = vec![];

    // Inspect unit
    let active_stop;
    let pid_relevant;
    let started_at_pid;
    match args.unit.rsplit(".").next().unwrap() {
        "service" => {
            pid_relevant = true;
            match systemctl_show_prop(&args.unit, "Type").await?.as_str() {
                "oneshot" => {
                    active_stop = systemctl_show_prop(&args.unit, "RemainAfterExit").await? == "yes";
                    started_at_pid = false;
                },
                "simple" | "exec" | "notify" | "forking" | "dbus" => {
                    active_stop = false;
                    started_at_pid = true;
                },
                type_ => {
                    return Err(loga::err(format!("Unhandled service type [{}]", type_)));
                },
            }
        },
        _ => {
            pid_relevant = false;
            started_at_pid = false;
            active_stop = true;
        },
    }

    // Clean systemd state to prevent it from getting in the way
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

    // Run and wait to exit
    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt()).context("Error hooking into SIGINT")?;
    let sigint = Box::pin(sigint.recv());
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate()).context("Error hooking into SIGTERM")?;
    let sigterm = Box::pin(sigterm.recv());
    select!{
        r = run_unit(&args.unit, UnitMeta {
            pid_relevant: pid_relevant,
            active_stop: active_stop,
            started_at_pid: started_at_pid,
        }) => {
            match r {
                Ok(_) => { },
                Err(e) => {
                    errors.push(e);
                },
            }
        },
        _ = sigint => {
        },
        _ = sigterm => {
        }
    };

    // Stop unit
    if let Err(e) = Command::new("systemctl").arg("stop").arg(&args.unit).output().await {
        errors.push(e.context("Error stopping unit"));
    }

    // Exit
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
