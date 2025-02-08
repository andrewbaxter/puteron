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
    rustix::{
        process::{
            pidfd_open,
            PidfdFlags,
        },
        termios::Pid,
    },
    std::{
        os::fd::OwnedFd,
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
        String::from_utf8(
            proc.stdout.clone(),
        ).context_with(
            format!("Found unit [{}] property [{}] value is not valid utf-8", unit, prop),
            ea!(value = String::from_utf8_lossy(&proc.stdout)),
        )?,
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
#[derive(Aargvark)]
struct Args {
    /// The unit to foreground
    unit: String,
    /// Oneshot - if the unit exits with a success exit code, don't stop the unit.
    oneshot: Option<()>,
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
    match Command::new("systemctl").arg("start").arg(&args.unit).output().await {
        Ok(_) => {
            match async {
                let wait_work = async {
                    loop {
                        match async {
                            ta_return!((), loga::Error);
                            let status =
                                systemctl_show_prop(&args.unit, "SubState")
                                    .await
                                    .context("Error getting unit result")?;
                            if status != "running" {
                                return Ok(());
                            }
                            let pid =
                                systemctl_show_prop(&args.unit, "MainPID")
                                    .await
                                    .context("Error querying unit PID")?;
                            if pid.is_empty() {
                                // No PID expected?.
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
                                let _: AsyncFdReadyGuard<OwnedFd> =
                                    pidfd
                                        .readable()
                                        .await
                                        .context_with(
                                            "Error waiting for pidfd to report readable",
                                            ea!(pid = pid.dbg_str()),
                                        )?;
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
    if errors.is_empty() && args.oneshot.is_some() {
        if let Err(e) = Command::new("systemctl").arg("stop").arg(&args.unit).output().await {
            errors.push(e.context("Error stopping unit"));
        }
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
