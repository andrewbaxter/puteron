use {
    super::{
        task_control::sanity_check,
        task_util::actual_set,
    },
    crate::{
        demon::{
            state::{
                State,
                StateDynamic,
                TaskStateSpecific,
            },
            task_control::{
                plan_event_started,
                plan_event_stopped,
                plan_event_stopping,
                plan_set_direct_off,
                plan_set_direct_on,
                sync_actual_should_start_downstream,
                sync_actual_should_stop_related,
                ExecutePlan,
            },
            task_create_delete::delete_task,
            task_util::{
                get_short_task_started_action,
                get_task,
                is_control_effective_on,
            },
        },
        interface::{
            self,
            base::TaskId,
            demon,
            ipc::Actual,
        },
        run_dir,
        time::{
            SimpleDuration,
            SimpleDurationUnit,
        },
    },
    flowcontrol::{
        exenum,
        shed,
        ta_return,
    },
    loga::{
        ea,
        DebugDisplay,
        ErrContext,
        Log,
        ResultContext,
    },
    rand::Rng,
    rustix::{
        process::Signal,
        termios::Pid,
    },
    std::{
        collections::HashSet,
        process::Stdio,
        sync::Arc,
        time::Duration,
        u32,
    },
    syslog::Formatter3164,
    tokio::{
        fs::remove_file,
        io::{
            AsyncBufReadExt,
            AsyncRead,
            BufReader,
        },
        net::TcpStream,
        process::{
            Child,
            Command,
        },
        select,
        sync::oneshot,
        task::spawn_blocking,
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

fn spawn_proc(
    state: &Arc<State>,
    task_id: &TaskId,
    spec: &interface::task::Command,
) -> Result<(Child, Pid), loga::Error> {
    // Prep command and args
    let mut command = Command::new("setsid");
    command.args(&spec.line);

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
    log.log_with(loga::DEBUG, "Spawning task process", ea!(task = task_id));

    // Pre-spawn logging setup
    command.stderr(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stdin(Stdio::null());

    // Launch
    let mut child = command.spawn().context("Failed to spawn subprocess")?;
    drop(command);
    let pid = Pid::from_raw(child.id().unwrap() as i32).unwrap();

    // Post-launch logging setup
    fn log_stream(
        state: &Arc<State>,
        log: &Log,
        task_id: &TaskId,
        stream: impl AsyncRead + 'static + Unpin + Send + Sync,
    ) -> Result<(), loga::Error> {
        let log = log.clone();
        let shutdown_timeout = {
            let cancel = state.shutdown.clone();
            let log = log.clone();
            async move {
                cancel.cancelled().await;
                sleep(Duration::from_secs(15)).await;
                log.log(loga::WARN, format!("Task logger didn't exit in 15s since shutdown, giving up."));
            }
        };
        let mut stream = LinesStream::new(BufReader::new(stream).lines());
        match &state.log_type {
            demon::LogType::Stderr => {
                state.tokio_tasks.spawn({
                    let task_id = task_id.clone();
                    async move {
                        let work = async move {
                            while let Some(line) = stream.next().await {
                                match async {
                                    ta_return!((), loga::Error);
                                    let line = line.context("Error receiving line from child process")?;
                                    eprintln!("[{}] {}", task_id, line);
                                    return Ok(());
                                }.await {
                                    Err(e) => {
                                        log.log_err(loga::WARN, e.context("Error joining blocking task"));
                                        break;
                                    },
                                    Ok(_) => { },
                                };
                            }
                        };
                        select!{
                            _ = shutdown_timeout => {
                            },
                            _ = work => {
                            }
                        }
                    }
                });
            },
            demon::LogType::Syslog => {
                state.tokio_tasks.spawn({
                    let mut syslogger = syslog::unix(Formatter3164 {
                        facility: syslog::Facility::LOG_USER,
                        process: task_id.clone(),
                        hostname: None,
                        pid: 0,
                    })?;
                    async move {
                        let work = async move {
                            while let Some(line) = stream.next().await {
                                match spawn_blocking(move || {
                                    match (|| {
                                        ta_return!((), loga::Error);
                                        let line = line.context("Error receiving line from child process")?;
                                        syslogger.info(line).context("Error sending child process line to syslog")?;
                                        return Ok(());
                                    })() {
                                        Ok(_) => {
                                            return (syslogger, Ok(()));
                                        },
                                        Err(e) => {
                                            return (syslogger, Err(e));
                                        },
                                    }
                                }).await {
                                    Err(e) => {
                                        log.log_err(
                                            loga::WARN,
                                            e.context(
                                                "Error joining blocking task; syslogger lost, aborting logging",
                                            ),
                                        );
                                        break;
                                    },
                                    Ok((l, Ok(_))) => {
                                        syslogger = l;
                                    },
                                    // Syslog restarting? or something
                                    Ok((l, Err(e))) => {
                                        log.log_err(loga::WARN, e.context("Error forwarding child output line"));
                                        syslogger = l;
                                    },
                                };
                            }
                        };
                        select!{
                            _ = shutdown_timeout => {
                            },
                            _ = work => {
                            }
                        }
                    }
                });
            },
        }
        return Ok(());
    }

    log_stream(state, &log, task_id, child.stdout.take().unwrap())?;
    log_stream(state, &log, task_id, child.stderr.take().unwrap())?;
    return Ok((child, pid));
}

async fn gentle_stop_proc(log: &Log, pid: Pid, mut child: Child, stop_timeout: Option<SimpleDuration>) {
    if let Err(e) = rustix::process::kill_process(pid, Signal::Term) {
        log.log_err(loga::WARN, e.context("Error sending SIGTERM to child"));
    }
    select!{
        r = child.wait() => {
            log.log(loga::INFO, format!("Process ended with status: {:?}", r));
        },
        _ = sleep(stop_timeout.map(|x| x.into()).unwrap_or(Duration::from_secs(30))) => {
            if let Err(e) = rustix::process::kill_process(pid, Signal::Kill) {
                log.log_err(loga::WARN, e.context("Error sending SIGKILL to child"));
            }
            log.log(loga::INFO, format!("Sent KILL: timeout after TERM"));
        }
    }
}

// For use during execute (as part of existing event atomic event - while already
// locked)
fn event_starting_actual(state: &Arc<State>, state_dynamic: &StateDynamic, task_id: &TaskId) {
    let task = get_task(&state_dynamic, task_id);

    // Update actual state
    actual_set(&state_dynamic, task, Actual::Starting);
    log_starting(state, task_id);
    // Nothing happens directly due to starting, no graph changes
}

fn event_starting(state: &Arc<State>, task_id: &TaskId) {
    let state_dynamic = state.dynamic.lock().unwrap();
    event_starting_actual(state, &state_dynamic, task_id);
}

/// For use during execute (as part of existing event atomic event - while already
/// locked).
///
/// Doesn't do graph changes, just update actual.
fn event_stopping_actual(state_dynamic: &StateDynamic, task_id: &TaskId, failed: bool) {
    let task = get_task(&state_dynamic, task_id);
    actual_set(&state_dynamic, task, Actual::Stopping);
    match &task.specific {
        TaskStateSpecific::Empty(_specific) => { },
        TaskStateSpecific::Long(specific) => {
            specific.pid.set(None);
            if failed {
                specific.failed_start_count.set(specific.failed_start_count.get() + 1);
            }
        },
        TaskStateSpecific::Short(specific) => {
            specific.pid.set(None);
            if failed {
                specific.failed_start_count.set(specific.failed_start_count.get() + 1);
            }
        },
    }
}

fn event_stopping(state: &Arc<State>, task_id: &TaskId, failed: bool) {
    let mut state_dynamic = state.dynamic.lock().unwrap();

    // Update actual state
    event_stopping_actual(&state_dynamic, task_id, failed);

    // Plan graph changes
    let mut plan = ExecutePlan::default();
    plan_event_stopping(&state_dynamic, &mut plan, task_id);

    // Execute graph changes
    execute(state, &mut state_dynamic, plan);
}

/// After state change
fn event_started(state: &Arc<State>, task_id: &TaskId) {
    let mut state_dynamic = state.dynamic.lock().unwrap();

    // Update actual
    let task = get_task(&state_dynamic, task_id);
    if task.actual.get().0 != Actual::Starting {
        // Long process, stop called during starting and reaches started state at the same
        // time
        return;
    }
    actual_set(&state_dynamic, task, Actual::Started);

    enum EventStartedAction {
        None,
        SetOff,
    }

    let extra_action;
    match &task.specific {
        TaskStateSpecific::Empty(_specific) => {
            extra_action = EventStartedAction::None;
        },
        TaskStateSpecific::Long(specific) => {
            extra_action = EventStartedAction::None;
            specific.failed_start_count.set(0);
        },
        TaskStateSpecific::Short(specific) => {
            specific.pid.set(None);
            specific.stop.borrow_mut().take();
            specific.failed_start_count.set(0);
            match get_short_task_started_action(&specific.spec) {
                interface::task::ShortTaskStartedAction::None => {
                    extra_action = EventStartedAction::None;
                },
                interface::task::ShortTaskStartedAction::TurnOff | interface::task::ShortTaskStartedAction::Delete => {
                    extra_action = EventStartedAction::SetOff;
                },
            }
        },
    }
    for waiter in task.stopped_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(false);
    }
    for waiter in task.started_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(true);
    }

    // Plan graph changes
    let mut plan = ExecutePlan::default();
    match extra_action {
        EventStartedAction::None => { },
        EventStartedAction::SetOff => {
            plan_set_direct_off(&state_dynamic, &mut plan, task_id);
        },
    }
    if !is_control_effective_on(task) {
        sync_actual_should_stop_related(&state_dynamic, &mut plan, task_id);
    } else {
        plan_event_started(&state_dynamic, &mut plan, task_id);
    }

    // Execute graph changes
    execute(state, &mut state_dynamic, plan);
}

/// After state change
fn event_stopped(state: &Arc<State>, task_id: &TaskId) {
    let mut state_dynamic = state.dynamic.lock().unwrap();

    // Actual changes
    let task = get_task(&state_dynamic, task_id);
    actual_set(&state_dynamic, task, Actual::Stopped);
    match &task.specific {
        TaskStateSpecific::Empty(_specific) => { },
        TaskStateSpecific::Long(specific) => {
            specific.pid.set(None);
        },
        TaskStateSpecific::Short(specific) => {
            specific.pid.set(None);
        },
    }
    log_stopped(state, task_id);
    for waiter in task.stopped_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(true);
    }
    for waiter in task.started_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(false);
    }

    // Plan graph changes
    let mut plan = ExecutePlan::default();
    if is_control_effective_on(task) {
        // Restart
        sync_actual_should_start_downstream(&state_dynamic, &mut plan, task_id, task);
    } else {
        plan_event_stopped(&state_dynamic, &mut plan, task_id);
    }

    // Execute graph changes
    execute(state, &mut state_dynamic, plan);
}

pub(crate) fn set_task_direct_on(state: &Arc<State>, state_dynamic: &mut StateDynamic, root_task_id: &TaskId) {
    let mut plan = ExecutePlan::default();
    plan_set_direct_on(state_dynamic, &mut plan, root_task_id);
    execute(state, state_dynamic, plan);
}

pub(crate) fn set_task_direct_off(state: &Arc<State>, state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    let mut plan = ExecutePlan::default();
    plan_set_direct_off(state_dynamic, &mut plan, task_id);
    execute(state, state_dynamic, plan);
}

struct RestartCounter {
    min: Duration,
    max: Duration,
    max_count: u32,
    count: u32,
}

impl RestartCounter {
    fn new(min: Option<SimpleDuration>, max: Option<SimpleDuration>) -> Self {
        let min = Duration::from(min.unwrap_or(SimpleDuration {
            count: 1,
            unit: SimpleDurationUnit::Minute,
        }).into());
        let max = max.map(<SimpleDuration as Into<Duration>>::into).unwrap_or(min).max(min);
        let max_count = (max.as_secs() as f64 / min.as_secs() as f64).log2().ceil() as u32;
        return RestartCounter {
            min: min,
            max: max,
            max_count: max_count,
            count: 0,
        };
    }

    fn reset(&mut self) {
        self.count = 0;
    }

    fn next_delay(&mut self) -> Duration {
        let duration = (self.min * 2u32.pow(self.count.min(self.max_count))).min(self.max);
        let duration =
            duration +
                Duration::from_secs_f64(
                    (rand::rng().random_range(0 .. u32::MAX) as f64 / (u32::MAX as f64)) * duration.as_secs_f64(),
                );
        self.count += 1;
        return duration;
    }
}

fn execute(state: &Arc<State>, state_dynamic: &mut StateDynamic, plan: ExecutePlan) {
    if state.debug {
        sanity_check(&state.log, state_dynamic, &plan);
    }
    for task_id in plan.log_starting {
        log_starting(&state, &task_id);
    }
    for task_id in plan.log_started {
        log_started(&state, &task_id);
    }
    for task_id in plan.log_stopping {
        log_stopping(&state, &task_id);
    }
    for task_id in plan.log_stopped {
        log_stopped(&state, &task_id);
    }
    for task_id in plan.run {
        let log = state.log.fork(ea!(task = task_id));
        event_starting_actual(&state, state_dynamic, &task_id);
        let task = get_task(state_dynamic, &task_id);
        match &task.specific {
            TaskStateSpecific::Empty(_) => unreachable!(),
            TaskStateSpecific::Long(s) => {
                // Start
                let (stop_tx, mut stop_rx) = oneshot::channel();
                *s.stop.borrow_mut() = Some(stop_tx);
                state.tokio_tasks.spawn({
                    let spec = s.spec.clone();
                    let task_id = task.id.clone();
                    let state = state.clone();
                    let log = log.clone();
                    async move {
                        let mut restart_counter = RestartCounter::new(spec.restart_delay, spec.restart_delay_max);
                        loop {
                            enum EndAction {
                                Break,
                                Retry,
                            }

                            let end_action: EndAction = async {
                                // Execute
                                match &spec.started_check {
                                    None => { },
                                    Some(c) => match c {
                                        interface::task::StartedCheck::TcpSocket(_) => {
                                            // nop
                                        },
                                        interface::task::StartedCheck::Path(c) => {
                                            remove_file(&c).await.ignore();
                                        },
                                        interface::task::StartedCheck::RunPath(c) => shed!{
                                            let c = run_dir().join(c);
                                            remove_file(&c).await.ignore();
                                        },
                                    },
                                }
                                let (mut child, pid) = match spawn_proc(&state, &task_id, &spec.command) {
                                    Ok(x) => x,
                                    Err(e) => {
                                        log.log_err(loga::WARN, e.context("Failed to launch process"));
                                        event_stopping(&state, &task_id, true);
                                        match stop_rx.try_recv() {
                                            Ok(_) => {
                                                // Signaled to stop
                                                return EndAction::Break;
                                            },
                                            Err(oneshot::error::TryRecvError::Empty) => {
                                                // No signal to stop
                                                return EndAction::Retry;
                                            },
                                            Err(oneshot::error::TryRecvError::Closed) => {
                                                // Shutting down, tasks dropped maybe?
                                                return EndAction::Break;
                                            },
                                        }
                                    },
                                };
                                {
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let specific =
                                        exenum!(
                                            &get_task(&state_dynamic, &task_id).specific,
                                            TaskStateSpecific:: Long(s) => s
                                        ).unwrap();
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
                                            interface::task::StartedCheck::RunPath(c) => shed!{
                                                let c = run_dir().join(c);
                                                loop {
                                                    if c.exists() {
                                                        break;
                                                    }
                                                    sleep(Duration::from_secs(1)).await;
                                                }
                                            },
                                        },
                                    }

                                    // Event handling
                                    restart_counter.reset();
                                    event_started(&state, &task_id);

                                    // Do nothing forever
                                    std::future::pending::<()>().await;
                                };

                                // Wait until event
                                select!{
                                    _ = live_work => {
                                        unreachable!();
                                    },
                                    r = child.wait() => {
                                        log.log(
                                            loga::INFO,
                                            format!("Process prematurely exited with status: {:?}", r),
                                        );
                                        event_stopping(&state, &task_id, true);
                                        return EndAction::Retry;
                                    },
                                    _ =& mut stop_rx => {
                                        gentle_stop_proc(&log, pid, child, spec.stop_timeout).await;
                                        return EndAction::Break;
                                    },
                                }
                            }.await;
                            match end_action {
                                EndAction::Break => {
                                    break;
                                },
                                EndAction::Retry => {
                                    // nop
                                },
                            }
                            select!{
                                _ = sleep(restart_counter.next_delay()) => {
                                },
                                _ =& mut stop_rx => {
                                    break;
                                }
                            }
                            event_starting(&state, &task_id);
                        }
                        event_stopped(&state, &task_id);
                    }
                });
            },
            TaskStateSpecific::Short(s) => {
                // Start
                let (stop_tx, mut stop_rx) = oneshot::channel();
                *s.stop.borrow_mut() = Some(stop_tx);
                state.tokio_tasks.spawn({
                    let spec = s.spec.clone();
                    let task_id = task.id.clone();
                    let state = state.clone();
                    let log = log.clone();
                    async move {
                        let mut restart_counter = RestartCounter::new(spec.restart_delay, spec.restart_delay_max);
                        let mut success_codes = HashSet::new();
                        success_codes.extend(spec.success_codes);
                        if success_codes.is_empty() {
                            success_codes.insert(0);
                        }
                        let stopped = loop {
                            struct EndActionBreak {
                                do_event_stopped: bool,
                            }

                            enum EndAction {
                                Break(EndActionBreak),
                                Retry,
                            }

                            let end_action: EndAction = async {
                                let (mut child, pid) = match spawn_proc(&state, &task_id, &spec.command) {
                                    Ok(x) => x,
                                    Err(e) => {
                                        log.log_err(loga::WARN, e.context("Failed to launch process"));
                                        event_stopping(&state, &task_id, false);
                                        match stop_rx.try_recv() {
                                            Ok(_) => {
                                                return EndAction::Break(EndActionBreak { do_event_stopped: true });
                                            },
                                            Err(e) => match e {
                                                oneshot::error::TryRecvError::Empty => {
                                                    return EndAction::Retry;
                                                },
                                                oneshot::error::TryRecvError::Closed => {
                                                    return EndAction::Break(
                                                        EndActionBreak { do_event_stopped: true },
                                                    );
                                                },
                                            },
                                        }
                                    },
                                };
                                {
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let specific =
                                        exenum!(
                                            &get_task(&state_dynamic, &task_id).specific,
                                            TaskStateSpecific:: Short(s) => s
                                        ).unwrap();
                                    specific.pid.set(Some(pid.as_raw_nonzero().get()));
                                }

                                // Wait for exit
                                select!{
                                    r = child.wait() => {
                                        match r {
                                            Ok(r) => {
                                                if r.code().filter(|c| success_codes.contains(c)).is_some() {
                                                    if let Ok(_) = stop_rx.try_recv() {
                                                        event_stopping(&state, &task_id, false);
                                                        return EndAction::Break(
                                                            EndActionBreak { do_event_stopped: true },
                                                        );
                                                    } else {
                                                        event_started(&state, &task_id);
                                                        return EndAction::Break(
                                                            EndActionBreak { do_event_stopped: false },
                                                        );
                                                    }
                                                } else {
                                                    log.log(
                                                        loga::INFO,
                                                        format!("Process exited with non-success result: {:?}", r),
                                                    );
                                                    event_stopping(&state, &task_id, true);
                                                    return EndAction::Retry;
                                                }
                                            },
                                            Err(e) => {
                                                log.log_err(
                                                    loga::INFO,
                                                    e.context("Process ended with unknown result"),
                                                );
                                                event_stopping(&state, &task_id, true);
                                                return EndAction::Retry;
                                            },
                                        }
                                    }
                                    _ =& mut stop_rx => {
                                        gentle_stop_proc(&log, pid, child, spec.stop_timeout).await;
                                        return EndAction::Break(EndActionBreak { do_event_stopped: true });
                                    }
                                };
                            }.await;
                            match end_action {
                                EndAction::Break(b) => {
                                    break b.do_event_stopped;
                                },
                                EndAction::Retry => {
                                    // nop
                                },
                            }
                            select!{
                                _ = sleep(restart_counter.next_delay()) => {
                                },
                                _ =& mut stop_rx => {
                                    break true;
                                }
                            }
                            event_starting(&state, &task_id);
                        };
                        if stopped {
                            event_stopped(&state, &task_id);
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
                    event_stopping_actual(&state_dynamic, &task_id, false);
                    _ = stop.send(());
                }
            },
            TaskStateSpecific::Short(specific) => {
                if let Some(stop) = specific.stop.take() {
                    event_stopping_actual(&state_dynamic, &task_id, false);
                    _ = stop.send(());
                }
            },
        }
    }
    for task_id in plan.delete {
        delete_task(state_dynamic, &task_id);
    }
}
