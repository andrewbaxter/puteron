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
            waitpid,
            WaitOptions,
            WaitStatus,
        },
        termios::Pid,
    },
    std::time::Duration,
    tokio::{
        process::Command,
        select,
        signal::unix::SignalKind,
        task::spawn_blocking,
        time::sleep,
    },
};

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
    /// The expected successful exit code for the process (defaults to 0).
    exit_code: Option<u32>,
}

async fn main1() -> Result<(), loga::Error> {
    let args = vark::<Args>();
    let expect_exit_code = args.exit_code.unwrap_or(0);
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
            eprintln!("Failed resetting unit status: {}", e);
        },
    }
    match Command::new("systemctl").arg("start").arg(&args.unit).output().await {
        Ok(_) => {
            match async {
                let wait_work = async {
                    ta_return!(Result < Option < WaitStatus >, loga:: Error >, loga::Error);
                    loop {
                        match async {
                            ta_return!(Option < Result < Option < WaitStatus >, loga:: Error >>, loga::Error);
                            let pid =
                                Command::new("systemctl")
                                    .arg("show")
                                    .arg("--property")
                                    .arg("MainPID")
                                    .arg("--value")
                                    .arg(&args.unit)
                                    .output()
                                    .await?;
                            if !pid.status.success() {
                                return Err(
                                    loga::err_with("Error querying unit PID", ea!(status = pid.status.dbg_str())),
                                );
                            }
                            if pid.stdout.is_empty() {
                                // No PID expected?.
                                std::future::pending::<()>().await;
                                unreachable!();
                            };
                            let pid =
                                String::from_utf8(
                                    pid.stdout.clone(),
                                ).context_with(
                                    "Found PID is not valid utf-8",
                                    ea!(pid = String::from_utf8_lossy(&pid.stdout)),
                                )?;
                            let pid =
                                i32::from_str_radix(
                                    &pid.trim_ascii(),
                                    10,
                                ).context_with("Found PID is not a valid i32", ea!(pid = pid))?;
                            if pid == 0 {
                                // Still starting up?
                                return Ok(None);
                            } else {
                                let pid =
                                    Pid::from_raw(
                                        pid,
                                    ).context_with("Found PID is not a valid PID (not positive)", ea!(pid = pid))?;
                                return Ok(
                                    Some(
                                        spawn_blocking(
                                            move || waitpid(Some(pid), WaitOptions::empty()).map_err(loga::err),
                                        )
                                            .await
                                            .map_err(loga::err)
                                            .and_then(|x| x),
                                    ),
                                );
                            }
                        }.await {
                            Ok(Some(status)) => {
                                return Ok(status);
                            },
                            Ok(None) => {
                                // nop
                            },
                            Err(e) => {
                                eprintln!("Error getting unit PID, retrying: {}", e);
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
                    wait_res = wait_work => {
                        let exit_code =
                            wait_res
                                .context("Error waiting for process wait thread")?
                                .context("Error retrieving process wait result")?
                                .context("Process wait ended with no result")?
                                .exit_status()
                                .context("Child exited with no status code")?;
                        if exit_code != expect_exit_code {
                            return Err(
                                loga::err(
                                    format!(
                                        "Child exited with failure exit code {}, expecting {}",
                                        exit_code,
                                        expect_exit_code
                                    ),
                                ),
                            );
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
