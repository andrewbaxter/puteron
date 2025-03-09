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
            task_create_delete::delete_task,
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
            task_util::{
                get_short_task_started_action,
                get_task,
                is_control_effective_on,
            },
        },
        interface::{
            self,
            base::TaskId,
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

    // Stdout/err -> syslog 1
    command.stderr(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stdin(Stdio::null());

    // Launch
    let mut child = command.spawn().context("Failed to spawn subprocess")?;
    drop(command);
    let pid = Pid::from_raw(child.id().unwrap() as i32).unwrap();

    // Stdout/err -> syslog 2
    fn log_stream(
        state: &Arc<State>,
        log: &Log,
        task_id: &TaskId,
        stream: impl AsyncRead + 'static + Unpin + Send + Sync,
    ) -> Result<(), loga::Error> {
        state.tokio_tasks.spawn({
            let mut syslogger = syslog::unix(Formatter3164 {
                facility: syslog::Facility::LOG_USER,
                process: task_id.clone(),
                hostname: None,
                pid: 0,
            })?;
            let log = log.clone();
            let mut stream = LinesStream::new(BufReader::new(stream).lines());
            let state = state.clone();
            async move {
                let shutdown_timeout = {
                    let cancel = state.shutdown.clone();
                    let log = log.clone();
                    async move {
                        cancel.cancelled().await;
                        sleep(Duration::from_secs(15)).await;
                        log.log(loga::WARN, format!("Task logger didn't exit in 15s since shutdown, giving up."));
                    }
                };
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
                                    e.context("Error joining blocking task; syslogger lost, aborting logging"),
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

fn event_starting(state: &Arc<State>, task_id: &TaskId) {
    log_starting(state, task_id);
}

fn event_stopping(state: &Arc<State>, state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    let mut plan = ExecutePlan::default();
    plan_event_stopping(state_dynamic, &mut plan, task_id);
    execute(state, state_dynamic, plan);
}

enum EventStartedAction {
    None,
    SetOff,
}

/// After state change
fn event_started(
    state: &Arc<State>,
    state_dynamic: &mut StateDynamic,
    task_id: &TaskId,
    extra_action: EventStartedAction,
) {
    let task = get_task(state_dynamic, task_id);
    for waiter in task.stopped_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(false);
    }
    for waiter in task.started_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(true);
    }
    let mut plan = ExecutePlan::default();
    match extra_action {
        EventStartedAction::None => { },
        EventStartedAction::SetOff => {
            plan_set_direct_off(state_dynamic, &mut plan, task_id);
        },
    }
    eprintln!("{} started - effective on {}", task_id, is_control_effective_on(task));
    if !is_control_effective_on(task) {
        sync_actual_should_stop_related(state_dynamic, &mut plan, task_id);
    } else {
        plan_event_started(state_dynamic, &mut plan, task_id);
    }
    execute(state, state_dynamic, plan);
}

/// After state change
fn event_stopped(state: &Arc<State>, state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    log_stopped(state, task_id);
    let task = get_task(state_dynamic, task_id);
    for waiter in task.stopped_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(true);
    }
    for waiter in task.started_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(false);
    }
    let mut plan = ExecutePlan::default();
    if is_control_effective_on(task) {
        // Restart
        sync_actual_should_start_downstream(state_dynamic, &mut plan, task_id, task);
    } else {
        plan_event_stopped(state_dynamic, &mut plan, task_id);
    }
    execute(state, state_dynamic, plan);
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

macro_rules! event_short_stopped2{
    // Work around borrow rules preventing code reuse
    ($state: expr, $state_dynamic: expr, $task_id: expr, $task: expr, $specific: expr) => {
        actual_set($state_dynamic, $task, Actual::Stopped);
        $specific.pid.set(None);
        event_stopped(&$state, $state_dynamic, &$task_id);
    };
}

fn event_short_stopped(state: &Arc<State>, task_id: &TaskId) {
    let mut state_dynamic = state.dynamic.lock().unwrap();
    let state_dynamic = &mut state_dynamic;
    let task = get_task(state_dynamic, &task_id);
    let specific = exenum!(&task.specific, TaskStateSpecific:: Short(s) => s).unwrap();
    event_short_stopped2!(state, state_dynamic, task_id, task, specific);
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

    fn failure_count(&self) -> usize {
        return self.count as usize + 1;
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
        let task = get_task(state_dynamic, &task_id);
        let log = state.log.fork(ea!(task = task.id));

        // Mark as starting
        actual_set(state_dynamic, task, Actual::Starting);
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
                            event_starting(&state, &task_id);

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
                                        match stop_rx.try_recv() {
                                            Ok(_) => {
                                                return EndAction::Break;
                                            },
                                            Err(e) => match e {
                                                oneshot::error::TryRecvError::Empty => {
                                                    {
                                                        let state_dynamic = state.dynamic.lock().unwrap();
                                                        let specific =
                                                            exenum!(
                                                                &get_task(&state_dynamic, &task_id).specific,
                                                                TaskStateSpecific:: Long(s) => s
                                                            ).unwrap();
                                                        specific
                                                            .failed_start_count
                                                            .set(restart_counter.failure_count());
                                                    }
                                                    return EndAction::Retry;
                                                },
                                                oneshot::error::TryRecvError::Closed => {
                                                    return EndAction::Break;
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
                                    {
                                        let mut state_dynamic = state.dynamic.lock().unwrap();
                                        let task = get_task(&state_dynamic, &task_id);
                                        actual_set(&state_dynamic, task, Actual::Started);
                                        let specific =
                                            exenum!(&task.specific, TaskStateSpecific:: Long(s) => s).unwrap();
                                        restart_counter.reset();
                                        specific.failed_start_count.set(0);
                                        event_started(&state, &mut state_dynamic, &task_id, EventStartedAction::None);
                                    }

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
                                        {
                                            let mut state_dynamic = state.dynamic.lock().unwrap();

                                            // Move through stopping
                                            event_stopping(&state, &mut state_dynamic, &task_id);

                                            // May or may not have started; mark as starting + do state updates
                                            let task = get_task(&state_dynamic, &task_id);
                                            if task.actual.get().0 != Actual::Starting {
                                                actual_set(&state_dynamic, task, Actual::Starting);
                                            }
                                            let specific =
                                                exenum!(&task.specific, TaskStateSpecific:: Long(s) => s).unwrap();
                                            specific.pid.set(None);
                                            specific.failed_start_count.set(restart_counter.failure_count());
                                        }
                                        return EndAction::Retry;
                                    },
                                    _ =& mut stop_rx => {
                                        // Mark as stopping + do state updates
                                        {
                                            let mut state_dynamic = state.dynamic.lock().unwrap();
                                            let task = get_task(&state_dynamic, &task_id);
                                            actual_set(&state_dynamic, task, Actual::Stopping);
                                            event_stopping(&state, &mut state_dynamic, &task_id);
                                        }

                                        // Signal stop
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
                        }

                        // Handle stopped
                        {
                            let mut state_dynamic = state.dynamic.lock().unwrap();
                            let task = get_task(&state_dynamic, &task_id);
                            actual_set(&state_dynamic, task, Actual::Stopped);
                            let specific = exenum!(&task.specific, TaskStateSpecific:: Long(s) => s).unwrap();
                            specific.pid.set(None);
                            event_stopped(&state, &mut state_dynamic, &task_id);
                        }
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
                            event_starting(&state, &task_id);

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
                                        let mut state_dynamic = state.dynamic.lock().unwrap();
                                        let task = get_task(&state_dynamic, &task_id);
                                        let specific =
                                            exenum!(&task.specific, TaskStateSpecific:: Short(s) => s).unwrap();
                                        specific.pid.set(None);
                                        match r {
                                            Ok(r) => {
                                                if r.code().filter(|c| success_codes.contains(c)).is_some() {
                                                    // Mark as started + do state updates
                                                    {
                                                        actual_set(&state_dynamic, task, Actual::Started);
                                                        specific.stop.borrow_mut().take();
                                                        specific.failed_start_count.set(0);
                                                        let event_extra_action;
                                                        match get_short_task_started_action(&specific.spec) {
                                                            interface::task::ShortTaskStartedAction::None => {
                                                                event_extra_action = EventStartedAction::None;
                                                            },
                                                            interface::task::ShortTaskStartedAction::TurnOff |
                                                            interface::task::ShortTaskStartedAction::Delete => {
                                                                event_extra_action = EventStartedAction::SetOff;
                                                            },
                                                        }
                                                        event_started(
                                                            &state,
                                                            &mut state_dynamic,
                                                            &task_id,
                                                            event_extra_action,
                                                        );
                                                        return EndAction::Break(
                                                            EndActionBreak { do_event_stopped: false },
                                                        );
                                                    }
                                                } else {
                                                    log.log(
                                                        loga::INFO,
                                                        format!("Process exited with non-success result: {:?}", r),
                                                    );
                                                    {
                                                        // Implicit drop: `specific` `task`.
                                                        //
                                                        // Stopping, move back to starting
                                                        let specific = exenum!(&get_task(&state_dynamic, &task_id).specific, TaskStateSpecific:: Short(s) => s).unwrap();
                                                        specific
                                                            .failed_start_count
                                                            .set(restart_counter.failure_count());
                                                    }
                                                    return EndAction::Retry;
                                                }
                                            },
                                            Err(e) => {
                                                log.log_err(
                                                    loga::INFO,
                                                    e.context("Process ended with unknown result"),
                                                );

                                                // Implicit drop: `specific` `task`
                                                //
                                                // Stopping, move back to starting
                                                let specific = exenum!(&get_task(&state_dynamic, &task_id).specific, TaskStateSpecific:: Short(s) => s).unwrap();
                                                specific.failed_start_count.set(restart_counter.failure_count());
                                                return EndAction::Retry;
                                            },
                                        }
                                    }
                                    _ =& mut stop_rx => {
                                        // Mark as stopping + before stopping
                                        {
                                            let state_dynamic = state.dynamic.lock().unwrap();
                                            let task = get_task(&state_dynamic, &task_id);
                                            actual_set(&state_dynamic, task, Actual::Stopping);
                                        }
                                        gentle_stop_proc(&log, pid, child, spec.stop_timeout).await;

                                        // Stopped
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
                        };
                        if stopped {
                            event_short_stopped(&state, &task_id);
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
                if task.actual.get().0 == Actual::Started {
                    event_short_stopped2!(state, state_dynamic, task_id, task, specific);
                }
            },
        }
    }
    for task_id in plan.delete {
        delete_task(state_dynamic, &task_id);
    }
}
