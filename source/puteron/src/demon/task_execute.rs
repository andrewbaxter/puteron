use {
    super::{
        state::{
            State,
            StateDynamic,
            TaskStateSpecific,
        },
        task_plan::{
            plan_event_started,
            plan_event_stopped,
            plan_event_stopping,
            plan_set_task_direct_off,
            plan_set_task_direct_on,
            ExecutePlan,
        },
        task_util::{
            get_short_task_started_action,
            is_task_started,
        },
    },
    crate::demon::{
        task_create_delete::delete_task,
        task_util::{
            get_task,
        },
    },
    chrono::Utc,
    flowcontrol::ta_return,
    loga::{
        ea,
        DebugDisplay,
        ErrContext,
        Log,
        ResultContext,
    },
    puteron_lib::{
        interface::{
            self,
            base::TaskId,
            message::v1::ProcState,
        },
        time::{
            SimpleDuration,
            SimpleDurationUnit,
        },
    },
    rustix::{
        process::Signal,
        termios::Pid,
    },
    std::{
        collections::HashSet,
        future::Future,
        pin::Pin,
        process::Stdio,
        sync::Arc,
        time::Duration,
    },
    syslog::Formatter3164,
    tokio::{
        io::{
            AsyncBufReadExt,
            BufReader,
        },
        net::TcpStream,
        process::{
            Child,
            Command,
        },
        select,
        sync::oneshot,
        task::JoinError,
        time::{
            sleep,
            timeout,
        },
    },
    tokio_stream::{
        wrappers::LinesStream,
        StreamExt,
    },
};

fn log_starting(state: &State, task_id: &TaskId) {
    state.log.log_with(loga::DEBUG, "State change: starting (0)", ea!(task = task_id));
}

fn log_started(state: &State, task_id: &TaskId) {
    state.log.log_with(loga::DEBUG, "State change: started (1)", ea!(task = task_id));
}

fn log_stopping(state: &State, task_id: &TaskId) {
    state.log.log_with(loga::DEBUG, "State change: stopping (2)", ea!(task = task_id));
}

fn log_stopped(state: &State, task_id: &TaskId) {
    state.log.log_with(loga::DEBUG, "State change: stopped (3)", ea!(task = task_id));
}

type LoggerRetFuture =
    Pin<
        Box<
            dyn

                    Future<
                        Output = Result<syslog::Logger<syslog::LoggerBackend, syslog::Formatter3164>, JoinError>,
                    > +
                    Send,
        >,
    >;

fn spawn_proc(
    state: &State,
    task_id: &TaskId,
    spec: &interface::task::Command,
) -> Result<(Child, Pid, LoggerRetFuture), loga::Error> {
    // Prep command and args
    let mut command = Command::new(&spec.command[0]);
    command.args(&spec.command[1..]);

    // Working dir
    match &spec.working_directory {
        Some(w) => {
            command.current_dir(w);
        },
        None => {
            command.current_dir("/");
        },
    }

    // Env vars
    command.env_clear();
    for (k, v) in &state.env {
        if !spec.environment.clean || spec.environment.keep.get(k).cloned().unwrap_or(false) {
            command.env(k, v);
        }
    }
    for (k, v) in &spec.environment.add {
        command.env(k, v);
    }
    let log = state.log.fork(ea!(command = command.dbg_str()));
    log.log(loga::DEBUG, "Spawning task process");

    // Stdout/err -> syslog 1
    command.stderr(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stdin(Stdio::null());

    // Launch
    command.process_group(0);
    let mut child = command.spawn().context("Failed to spawn subprocess")?;
    drop(command);
    let pid = Pid::from_raw(child.id().unwrap() as i32).unwrap();

    // Stdout/err -> syslog 2
    let logger = Box::pin(state.tokio_tasks.spawn({
        let stdout = LinesStream::new(BufReader::new(child.stdout.take().unwrap()).lines());
        let stderr = LinesStream::new(BufReader::new(child.stderr.take().unwrap()).lines());
        let mut combined_output = StreamExt::merge(stdout, stderr);
        let mut logger = syslog::unix(Formatter3164 {
            facility: syslog::Facility::LOG_USER,
            process: task_id.clone(),
            hostname: None,
            pid: 0,
        })?;
        async move {
            while let Some(line) = combined_output.next().await {
                match (|| {
                    ta_return!((), loga::Error);
                    let line = line.context("Error receiving line from child process")?;
                    logger.info(line).context("Error sending child process line to syslog")?;
                    return Ok(());
                })() {
                    Ok(_) => (),
                    // Syslog restarting? or something
                    Err(e) => {
                        log.log_err(loga::WARN, e.context("Error forwarding child output line"));
                    },
                };
            }
            return logger;
        }
    })) as LoggerRetFuture;
    return Ok((child, pid, logger));
}

async fn gentle_stop_proc(
    log: &Log,
    pid: Pid,
    mut child: Child,
    logger: LoggerRetFuture,
    stop_timeout: Option<SimpleDuration>,
) -> Result<(), loga::Error> {
    if let Err(e) = rustix::process::kill_process(pid, Signal::Term) {
        log.log_err(loga::WARN, e.context("Error sending SIGTERM to child"));
    }
    select!{
        r = child.wait() => {
            let mut logger = logger.await?;
            if let Err(e) = logger.info(format!("Process ended with status: {:?}", r)) {
                log.log_err(loga::WARN, e.context("Error sending message to syslog"));
            }
        },
        _ = sleep(stop_timeout.map(|x| x.into()).unwrap_or(Duration::from_secs(30))) => {
            if let Err(e) = rustix::process::kill_process(pid, Signal::Kill) {
                log.log_err(loga::WARN, e.context("Error sending SIGKILL to child"));
            }
            let mut logger = logger.await?;
            if let Err(e) = logger.info(format!("Sent KILL: timeout after TERM")) {
                log.log_err(loga::WARN, e.context("Error sending message to syslog"));
            }
        }
    }
    return Ok(());
}

fn event_starting(state: &Arc<State>, task_id: &TaskId) {
    log_starting(state, task_id);
}

fn event_stopping(state: &Arc<State>, state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    let mut plan = ExecutePlan::default();
    plan_event_stopping(state_dynamic, &mut plan, task_id);
    execute(state, state_dynamic, plan);
}

/// After state change
fn event_started(state: &Arc<State>, state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    log_started(state, task_id);
    let mut plan = ExecutePlan::default();
    plan_event_started(state_dynamic, &mut plan, task_id);
    execute(state, state_dynamic, plan);
    let task = get_task(state_dynamic, task_id);
    for waiter in task.stopped_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(false);
    }
    for waiter in task.started_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(true);
    }
}

/// After state change
fn event_stopped(state: &Arc<State>, state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    log_stopped(state, task_id);
    let mut plan = ExecutePlan::default();
    plan_event_stopped(state_dynamic, &mut plan, task_id);
    execute(state, state_dynamic, plan);
    let task = get_task(state_dynamic, task_id);
    for waiter in task.stopped_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(true);
    }
    for waiter in task.started_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(false);
    }
}

pub(crate) fn set_task_user_on(state: &Arc<State>, state_dynamic: &mut StateDynamic, root_task_id: &TaskId) {
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_on(state_dynamic, &mut plan, root_task_id);
    execute(state, state_dynamic, plan);
}

pub(crate) fn set_task_user_off(state: &Arc<State>, state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    let mut plan = ExecutePlan::default();
    plan_set_task_direct_off(state_dynamic, &mut plan, task_id);
    execute(state, state_dynamic, plan);
}

fn execute(state: &Arc<State>, state_dynamic: &mut StateDynamic, plan: ExecutePlan) {
    for task_id in plan.log_starting {
        log_starting(&state, &task_id);
    }
    for task_id in plan.log_stopping {
        log_stopping(&state, &task_id);
    }
    for task_id in plan.start {
        let task = get_task(state_dynamic, &task_id);
        let log = state.log.fork(ea!(task = task.id));
        match &task.specific {
            TaskStateSpecific::Empty(_) => unreachable!(),
            TaskStateSpecific::Long(s) => {
                // Mark as starting
                s.state.set((ProcState::Starting, Utc::now()));

                // Start
                let (stop_tx, mut stop_rx) = oneshot::channel();
                *s.stop.borrow_mut() = Some(stop_tx);
                state.tokio_tasks.spawn({
                    let spec = s.spec.clone();
                    let task_id = task.id.clone();
                    let state = state.clone();
                    let log = log.clone();
                    async move {
                        let restart_delay = Duration::from(spec.restart_delay.unwrap_or(SimpleDuration {
                            count: 1,
                            unit: SimpleDurationUnit::Minute,
                        }).into());
                        loop {
                            event_starting(&state, &task_id);
                            match async {
                                ta_return!(bool, loga::Error);

                                // Execute
                                let (mut child, pid, logger) = spawn_proc(&state, &task_id, &spec.command)?;
                                {
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let task = get_task(&state_dynamic, &task_id);
                                    let TaskStateSpecific::Long(specific) = &task.specific else {
                                        panic!();
                                    };
                                    specific.pid.set(Some(pid.as_raw_nonzero().get()));
                                }

                                // Wait until started
                                let live_work = async {
                                    // Started check
                                    match &spec.started_check {
                                        None => { },
                                        Some(c) => match c {
                                            interface::task::StartedCheck::TcpSocket(addr) => {
                                                loop {
                                                    if timeout(Duration::from_secs(1), TcpStream::connect(addr))
                                                        .await
                                                        .is_ok() {
                                                        break;
                                                    }
                                                    sleep(Duration::from_secs(1)).await;
                                                }
                                            },
                                            interface::task::StartedCheck::Path(c) => {
                                                loop {
                                                    if c.exists() {
                                                        break;
                                                    }
                                                    sleep(Duration::from_secs(1)).await;
                                                }
                                            },
                                        },
                                    }
                                    {
                                        let mut state_dynamic = state.dynamic.lock().unwrap();
                                        let task = get_task(&state_dynamic, &task_id);
                                        let TaskStateSpecific::Long(specific) = &task.specific else {
                                            panic!();
                                        };
                                        specific.state.set((ProcState::Started, Utc::now()));
                                        specific.failed_start_count.set(0);
                                        event_started(&state, &mut state_dynamic, &task_id);
                                    }

                                    // Do nothing forever
                                    loop {
                                        sleep(Duration::MAX).await;
                                    }
                                };

                                // Wait until event
                                select!{
                                    _ = live_work => {
                                        unreachable!();
                                    },
                                    _ =& mut stop_rx => {
                                        // Mark as stopping + do state updates
                                        {
                                            let mut state_dynamic = state.dynamic.lock().unwrap();
                                            let task = get_task(&state_dynamic, &task_id);
                                            let TaskStateSpecific::Long(specific) = &task.specific else {
                                                panic!();
                                            };
                                            specific.state.set((ProcState::Stopping, Utc::now()));
                                            event_stopping(&state, &mut state_dynamic, &task_id);
                                        }

                                        // Signal stop
                                        gentle_stop_proc(&log, pid, child, logger, spec.stop_timeout).await?;

                                        // Mark as stopped
                                        {
                                            let mut state_dynamic = state.dynamic.lock().unwrap();
                                            let task = get_task(&state_dynamic, &task_id);
                                            let TaskStateSpecific::Long(specific) = &task.specific else {
                                                panic!();
                                            };
                                            specific.state.set((ProcState::Stopped, Utc::now()));
                                            specific.pid.set(None);
                                            event_stopped(&state, &mut state_dynamic, &task_id);
                                        }
                                        return Ok(true);
                                    },
                                    r = child.wait() => {
                                        let mut logger = logger.await?;
                                        if let Err(e) = logger.info(format!("Process ended with status: {:?}", r)) {
                                            log.log_err(loga::WARN, e.context("Error sending message to syslog"));
                                        }
                                        {
                                            let mut state_dynamic = state.dynamic.lock().unwrap();

                                            // Move through stopping
                                            event_stopping(&state, &mut state_dynamic, &task_id);

                                            // Mark as starting + do state updates
                                            let task = get_task(&state_dynamic, &task_id);
                                            let TaskStateSpecific::Long(specific) = &task.specific else {
                                                panic!();
                                            };
                                            specific.state.set((ProcState::Starting, Utc::now()));
                                        }
                                        return Ok(false);
                                    }
                                }
                            }.await {
                                Ok(done) => {
                                    if done {
                                        break;
                                    }
                                },
                                Err(e) => {
                                    log.log_err(loga::WARN, e.context("Long task process failed with error"));
                                },
                            }
                            select!{
                                _ = sleep(restart_delay) => {
                                },
                                _ =& mut stop_rx => {
                                    // Do stopped transition
                                    {
                                        let mut state_dynamic = state.dynamic.lock().unwrap();
                                        let TaskStateSpecific::Long(specific) =
                                            &get_task(&state_dynamic, &task_id).specific else {
                                                panic!();
                                            };
                                        specific.state.set((ProcState::Stopped, Utc::now()));
                                        event_stopped(&state, &mut state_dynamic, &task_id);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                });
            },
            TaskStateSpecific::Short(s) => {
                // Mark as starting
                s.state.set((ProcState::Starting, Utc::now()));

                // Start
                let (stop_tx, mut stop_rx) = oneshot::channel();
                *s.stop.borrow_mut() = Some(stop_tx);
                state.tokio_tasks.spawn({
                    let spec = s.spec.clone();
                    let task_id = task.id.clone();
                    let state = state.clone();
                    let log = log.clone();
                    async move {
                        let restart_delay = Duration::from(spec.restart_delay.unwrap_or(SimpleDuration {
                            count: 1,
                            unit: SimpleDurationUnit::Minute,
                        }).into());
                        let mut success_codes = HashSet::new();
                        success_codes.extend(spec.success_codes);
                        if success_codes.is_empty() {
                            success_codes.insert(0);
                        }
                        loop {
                            event_starting(&state, &task_id);
                            match async {
                                ta_return!(bool, loga::Error);
                                let (mut child, pid, logger) = spawn_proc(&state, &task_id, &spec.command)?;
                                {
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let task = get_task(&state_dynamic, &task_id);
                                    let TaskStateSpecific::Short(specific) = &task.specific else {
                                        panic!();
                                    };
                                    specific.pid.set(Some(pid.as_raw_nonzero().get()));
                                }

                                // Wait for exit
                                select!{
                                    r = child.wait() => {
                                        let mut logger = logger.await?;
                                        let mut state_dynamic = state.dynamic.lock().unwrap();
                                        let task = get_task(&state_dynamic, &task_id);
                                        let TaskStateSpecific::Short(specific) = &task.specific else {
                                            panic!();
                                        };
                                        specific.pid.set(None);
                                        match r {
                                            Ok(r) => {
                                                if r.code().filter(|c| success_codes.contains(c)).is_some() {
                                                    // Mark as started + do state updates
                                                    {
                                                        specific.stop.borrow_mut().take();
                                                        specific.failed_start_count.set(0);
                                                        specific.state.set((ProcState::Started, Utc::now()));
                                                        let started_action =
                                                            get_short_task_started_action(&specific.spec);
                                                        event_started(&state, &mut state_dynamic, &task_id);
                                                        match started_action {
                                                            interface::task::ShortTaskStartedAction::None => { },
                                                            interface::task::ShortTaskStartedAction::TurnOff |
                                                            interface::task::ShortTaskStartedAction::Delete => {
                                                                set_task_user_off(
                                                                    &state,
                                                                    &mut state_dynamic,
                                                                    &task_id,
                                                                );
                                                            },
                                                        }
                                                    }
                                                    return Ok(true);
                                                } else {
                                                    if let Err(e) =
                                                        logger.info(format!("Process ended with result: {:?}", r)) {
                                                        log.log_err(
                                                            loga::WARN,
                                                            e.context("Error sending message to syslog"),
                                                        );
                                                    }
                                                    {
                                                        // Implicit drop: `specific` `task`
                                                        event_stopping(&state, &mut state_dynamic, &task_id);

                                                        // Stopping, move back to starting
                                                        let task = get_task(&state_dynamic, &task_id);
                                                        let TaskStateSpecific::Short(specific) = &task.specific else {
                                                            panic!();
                                                        };
                                                        specific.state.set((ProcState::Starting, Utc::now()));
                                                        specific
                                                            .failed_start_count
                                                            .set(specific.failed_start_count.get() + 1);
                                                        event_starting(&state, &task_id);
                                                    }
                                                    return Ok(false);
                                                }
                                            },
                                            Err(e) => {
                                                if let Err(e) =
                                                    logger.info(
                                                        format!("Process ended with unknown result: {:?}", e),
                                                    ) {
                                                    log.log_err(
                                                        loga::WARN,
                                                        e.context("Error sending message to syslog"),
                                                    );
                                                };

                                                // Implicit drop: `specific` `task`
                                                event_stopping(&state, &mut state_dynamic, &task_id);

                                                // Stopping, move back to starting
                                                let task = get_task(&state_dynamic, &task_id);
                                                let TaskStateSpecific::Short(specific) = &task.specific else {
                                                    panic!();
                                                };
                                                specific.state.set((ProcState::Starting, Utc::now()));
                                                specific
                                                    .failed_start_count
                                                    .set(specific.failed_start_count.get() + 1);
                                                return Ok(false);
                                            },
                                        }
                                    }
                                    _ =& mut stop_rx => {
                                        // Mark as stopping + do state updates
                                        {
                                            let mut state_dynamic = state.dynamic.lock().unwrap();
                                            let TaskStateSpecific::Short(specific) =
                                                &get_task(&state_dynamic, &task_id).specific else {
                                                    panic!();
                                                };
                                            specific.state.set((ProcState::Stopping, Utc::now()));
                                            event_stopping(&state, &mut state_dynamic, &task_id);
                                        }

                                        // Signal stop
                                        gentle_stop_proc(&log, pid, child, logger, spec.stop_timeout).await?;

                                        // Mark as stopped
                                        {
                                            let mut state_dynamic = state.dynamic.lock().unwrap();
                                            let TaskStateSpecific::Short(specific) =
                                                &get_task(&state_dynamic, &task_id).specific else {
                                                    panic!();
                                                };
                                            specific.state.set((ProcState::Stopped, Utc::now()));
                                            specific.pid.set(None);
                                            let started_action = get_short_task_started_action(&specific.spec);
                                            event_stopped(&state, &mut state_dynamic, &task_id);
                                            if started_action == interface::task::ShortTaskStartedAction::Delete {
                                                delete_task(&mut state_dynamic, &task_id);
                                            }
                                        }
                                        return Ok(true);
                                    }
                                };
                            }.await {
                                Ok(done) => {
                                    if done {
                                        break;
                                    }
                                },
                                Err(e) => {
                                    log.log_err(loga::WARN, e.context("Short process failed with error"));
                                },
                            }
                            select!{
                                _ = sleep(restart_delay) => {
                                },
                                _ =& mut stop_rx => {
                                    // Do stopped transition
                                    {
                                        let mut state_dynamic = state.dynamic.lock().unwrap();
                                        let task = get_task(&state_dynamic, &task_id);
                                        let TaskStateSpecific::Short(specific) = &task.specific else {
                                            panic!();
                                        };
                                        specific.state.set((ProcState::Stopped, Utc::now()));
                                        let started_action = get_short_task_started_action(&specific.spec);
                                        event_stopped(&state, &mut state_dynamic, &task_id);
                                        if started_action == interface::task::ShortTaskStartedAction::Delete {
                                            delete_task(&mut state_dynamic, &task_id);
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                    }
                });
            },
        }
    }
    for task_id in plan.stop {
        let task = get_task(state_dynamic, &task_id);
        match &task.specific {
            TaskStateSpecific::Empty(_) => unreachable!(),
            TaskStateSpecific::Long(specific) => {
                if let Some(stop) = specific.stop.take() {
                    _ = stop.send(());
                }
            },
            TaskStateSpecific::Short(specific) => {
                if let Some(stop) = specific.stop.take() {
                    _ = stop.send(());
                }
                if is_task_started(task) {
                    // Already exited, handle task end
                    specific.state.set((ProcState::Stopped, Utc::now()));
                    let started_action = get_short_task_started_action(&specific.spec);
                    event_stopped(&state, state_dynamic, &task_id);
                    if started_action == interface::task::ShortTaskStartedAction::Delete {
                        delete_task(state_dynamic, &task_id);
                    }
                }
            },
        }
    }
}
