use {
    super::{
        state::{
            State,
            StateDynamic,
            TaskStateSpecific,
            TaskState_,
        },
        task_util::get_task,
    },
    crate::demon::{
        task_create_delete::delete_task,
        task_util::upstream,
    },
    chrono::Utc,
    flowcontrol::ta_return,
    loga::ResultContext,
    puteron_lib::{
        interface::{
            self,
            base::TaskId,
            message::v1::ProcState,
            task::DependencyType,
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
        collections::{
            HashSet,
        },
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
    tracing::{
        debug,
        info_span,
        warn,
        Instrument,
    },
};

pub(crate) fn log_starting(task_id: &TaskId) {
    //. debug!(task = task_id, "State change: starting");
    eprintln!("[{}] State change: starting", task_id);
}

pub(crate) fn log_started(task_id: &TaskId) {
    //. debug!(task = task_id, "State change: started");
    eprintln!("[{}] State change: started", task_id);
}

pub(crate) fn log_stopping(task_id: &TaskId) {
    //. debug!(task = task_id, "State change: stopping");
    eprintln!("[{}] State change: stopping", task_id);
}

pub(crate) fn log_stopped(task_id: &TaskId) {
    //. debug!(task = task_id, "State change: stopped");
    eprintln!("[{}] State change: stopped", task_id);
}

pub(crate) fn task_on(t: &TaskState_) -> bool {
    return t.user_on.get().0 || t.transitive_on.get().0;
}

pub(crate) fn task_started(t: &TaskState_) -> bool {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return s.started.get().0,
        TaskStateSpecific::Long(s) => {
            return s.state.get().0 == ProcState::Started;
        },
        TaskStateSpecific::Short(s) => {
            return s.state.get().0 == ProcState::Started;
        },
        TaskStateSpecific::External => unreachable!(),
    }
}

pub(crate) fn all_upstream_tasks_started(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    let mut all_started = true;
    upstream(task, |deps| {
        for (task_id, _) in deps {
            let Some(dep) = state_dynamic.tasks.get(task_id) else {
                all_started = false;
                return;
            };
            if !task_started(&state_dynamic.task_alloc[*dep]) {
                all_started = false;
                return;
            }
        }
    });
    return all_started;
}

pub(crate) fn task_stopped(t: &TaskState_) -> bool {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return !s.started.get().0,
        TaskStateSpecific::Long(s) => {
            return s.state.get().0 == ProcState::Stopped;
        },
        TaskStateSpecific::Short(s) => {
            return s.state.get().0 == ProcState::Stopped;
        },
        TaskStateSpecific::External => unreachable!(),
    }
}

pub(crate) fn all_downstream_tasks_stopped(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    for (task_id, _) in task.downstream.borrow().iter() {
        if !task_stopped(get_task(state_dynamic, task_id)) {
            return false;
        }
    }
    return true;
}

/// After state change
fn on_started(state: &Arc<State>, state_dynamic: &StateDynamic, task_id: &TaskId) {
    log_started(task_id);
    propagate_start_downstream(state, state_dynamic, task_id);
    let task = get_task(state_dynamic, task_id);
    for waiter in task.stopped_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(false);
    }
    for waiter in task.started_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(true);
    }
}

/// After state change
pub(crate) fn on_stopped(state_dynamic: &StateDynamic, task_id: &TaskId) {
    log_stopped(task_id);
    propagate_stop_upstream(state_dynamic, task_id);
    let task = get_task(state_dynamic, task_id);
    for waiter in task.stopped_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(true);
    }
    for waiter in task.started_waiters.borrow_mut().split_off(0) {
        _ = waiter.send(false);
    }
}

/// Return true if started - downstream can be started now.
pub(crate) fn do_start_task(state: &Arc<State>, state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    if !all_upstream_tasks_started(&state_dynamic, task) {
        return false;
    }
    if task_started(task) {
        return true;
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
        debug!(command =? command, "Spawning task process");

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
                            warn!(err = e.to_string(), "Error forwarding child output line");
                        },
                    };
                }
                return logger;
            }
        })) as LoggerRetFuture;
        return Ok((child, pid, logger));
    }

    async fn gentle_stop_proc(
        pid: Pid,
        mut child: Child,
        logger: LoggerRetFuture,
        stop_timeout: Option<SimpleDuration>,
    ) -> Result<(), loga::Error> {
        if let Err(e) = rustix::process::kill_process(pid, Signal::Term) {
            warn!(err = e.to_string(), "Error sending TERM to child");
        }
        select!{
            r = child.wait() => {
                let mut logger = logger.await?;
                if let Err(e) = logger.info(format!("Process ended with status: {:?}", r)) {
                    warn!(err = e.to_string(), "Error sending message to syslog");
                }
            },
            _ = sleep(stop_timeout.map(|x| x.into()).unwrap_or(Duration::from_secs(30))) => {
                if let Err(e) = rustix::process::kill_process(pid, Signal::Kill) {
                    warn!(err = e.to_string(), "Error sending KILL to child");
                }
                let mut logger = logger.await?;
                if let Err(e) = logger.info(format!("Sent KILL: timeout after TERM")) {
                    warn!(err = e.to_string(), "Error sending message to syslog");
                }
            }
        }
        return Ok(());
    }

    fn on_stopping(state_dynamic: &StateDynamic, task_id: &TaskId) {
        log_stopping(task_id);

        // Stop all downstream immediately
        let mut frontier = vec![];
        frontier.extend(get_task(state_dynamic, task_id).downstream.borrow().keys().cloned());
        while let Some(upstream_id) = frontier.pop() {
            let upstream_task = get_task(state_dynamic, &upstream_id);
            do_stop_task(state_dynamic, &upstream_task);
            frontier.extend(upstream_task.downstream.borrow().keys().cloned());
        }
    }

    /// After state changes
    fn on_starting(task_id: &TaskId) {
        log_starting(task_id);
    }

    match &task.specific {
        TaskStateSpecific::Empty(s) => {
            on_starting(&task.id);
            s.started.set((true, Utc::now()));
            on_started(state, state_dynamic, &task.id);
            return true;
        },
        TaskStateSpecific::Long(s) => {
            if s.state.get().0 != ProcState::Stopped {
                return false;
            }

            // Mark as starting
            s.state.set((ProcState::Starting, Utc::now()));

            // Start
            let (stop_tx, mut stop_rx) = oneshot::channel();
            *s.stop.borrow_mut() = Some(stop_tx);
            state.tokio_tasks.spawn({
                let spec = s.spec.clone();
                let task_id = task.id.clone();
                let state = state.clone();
                async move {
                    let restart_delay = Duration::from(spec.restart_delay.unwrap_or(SimpleDuration {
                        count: 1,
                        unit: SimpleDurationUnit::Minute,
                    }).into());
                    loop {
                        on_starting(&task_id);
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
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let task = get_task(&state_dynamic, &task_id);
                                    let TaskStateSpecific::Long(specific) = &task.specific else {
                                        panic!();
                                    };
                                    specific.state.set((ProcState::Started, Utc::now()));
                                    specific.failed_start_count.set(0);
                                    on_started(&state, &state_dynamic, &task_id);
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
                                        let state_dynamic = state.dynamic.lock().unwrap();
                                        let task = get_task(&state_dynamic, &task_id);
                                        let TaskStateSpecific::Long(specific) = &task.specific else {
                                            panic!();
                                        };
                                        specific.state.set((ProcState::Stopping, Utc::now()));
                                        on_stopping(&state_dynamic, &task_id);
                                    }

                                    // Signal stop
                                    gentle_stop_proc(pid, child, logger, spec.stop_timeout).await?;

                                    // Mark as stopped
                                    {
                                        let state_dynamic = state.dynamic.lock().unwrap();
                                        let task = get_task(&state_dynamic, &task_id);
                                        let TaskStateSpecific::Long(specific) = &task.specific else {
                                            panic!();
                                        };
                                        specific.state.set((ProcState::Stopped, Utc::now()));
                                        specific.pid.set(None);
                                        on_stopped(&state_dynamic, &task_id);
                                    }
                                    return Ok(true);
                                },
                                r = child.wait() => {
                                    let mut logger = logger.await?;
                                    if let Err(e) = logger.info(format!("Process ended with status: {:?}", r)) {
                                        warn!(err = e.to_string(), "Error sending message to syslog");
                                    }
                                    {
                                        let state_dynamic = state.dynamic.lock().unwrap();

                                        // Move through stopping
                                        on_stopping(&state_dynamic, &task_id);

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
                                warn!(err = e.to_string(), "Long process failed with error");
                            },
                        }
                        select!{
                            _ = sleep(restart_delay) => {
                            },
                            _ =& mut stop_rx => {
                                // Do stopped transition
                                {
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let TaskStateSpecific::Long(specific) =
                                        &get_task(&state_dynamic, &task_id).specific else {
                                            panic!();
                                        };
                                    specific.state.set((ProcState::Stopped, Utc::now()));
                                    on_stopped(&state_dynamic, &task_id);
                                }
                                break;
                            }
                        }
                    }
                }
            }.instrument(info_span!("task_long", task_id = task.id)));
            return false;
        },
        TaskStateSpecific::Short(s) => {
            if s.state.get().0 != ProcState::Stopped {
                return false;
            }

            // Mark as starting
            s.state.set((ProcState::Starting, Utc::now()));

            // Start
            let (stop_tx, mut stop_rx) = oneshot::channel();
            *s.stop.borrow_mut() = Some(stop_tx);
            state.tokio_tasks.spawn({
                let spec = s.spec.clone();
                let task_id = task.id.clone();
                let state = state.clone();
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
                        on_starting(&task_id);
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
                                    match r {
                                        Ok(r) => {
                                            if r.code().filter(|c| success_codes.contains(c)).is_some() {
                                                // Mark as started + do state updates
                                                {
                                                    let mut state_dynamic = state.dynamic.lock().unwrap();
                                                    let task = get_task(&state_dynamic, &task_id);
                                                    let TaskStateSpecific::Short(specific) = &task.specific else {
                                                        panic!();
                                                    };
                                                    specific.failed_start_count.set(0);
                                                    let started_action = match specific.spec.started_action {
                                                        None => {
                                                            if specific.spec.schedule.is_empty() {
                                                                interface::task::ShortTaskStartedAction::None
                                                            } else {
                                                                interface::task::ShortTaskStartedAction::TurnOff
                                                            }
                                                        },
                                                        Some(a) => a,
                                                    };
                                                    specific.state.set((ProcState::Started, Utc::now()));
                                                    match started_action {
                                                        interface::task::ShortTaskStartedAction::None => {
                                                            on_started(&state, &state_dynamic, &task_id);
                                                        },
                                                        interface::task::ShortTaskStartedAction::TurnOff |
                                                        interface::task::ShortTaskStartedAction::Delete => {
                                                            task.user_on.set((false, Utc::now()));
                                                            propagate_transitive_off(&state_dynamic, &task_id);
                                                            log_started(&task_id);
                                                            log_stopping(&task_id);
                                                            specific.state.set((ProcState::Stopped, Utc::now()));
                                                            specific.pid.set(None);
                                                            on_stopped(&state_dynamic, &task_id);
                                                            if started_action ==
                                                                interface::task::ShortTaskStartedAction::Delete {
                                                                delete_task(&mut state_dynamic, &task_id);
                                                            }
                                                        },
                                                    }
                                                }
                                                return Ok(true);
                                            } else {
                                                let mut logger = logger.await?;
                                                if let Err(e) =
                                                    logger.info(format!("Process ended with result: {:?}", r)) {
                                                    warn!(err = e.to_string(), "Error sending message to syslog");
                                                }
                                                {
                                                    let state_dynamic = state.dynamic.lock().unwrap();
                                                    let task = get_task(&state_dynamic, &task_id);
                                                    let TaskStateSpecific::Short(specific) = &task.specific else {
                                                        panic!();
                                                    };

                                                    // Stopping
                                                    on_stopping(&state_dynamic, &task_id);

                                                    // Move back to starting
                                                    specific.state.set((ProcState::Starting, Utc::now()));
                                                    specific
                                                        .failed_start_count
                                                        .set(specific.failed_start_count.get() + 1);
                                                    on_starting(&task_id);
                                                }
                                                return Ok(false);
                                            }
                                        },
                                        Err(e) => {
                                            let mut logger = logger.await?;
                                            if let Err(e) =
                                                logger.info(format!("Process ended with unknown result: {:?}", e)) {
                                                warn!(err = e.to_string(), "Error sending message to syslog");
                                            };

                                            // Keep as `starting`
                                            return Ok(false);
                                        },
                                    }
                                }
                                _ =& mut stop_rx => {
                                    // Mark as stopping + do state updates
                                    {
                                        let state_dynamic = state.dynamic.lock().unwrap();
                                        let TaskStateSpecific::Short(specific) =
                                            &get_task(&state_dynamic, &task_id).specific else {
                                                panic!();
                                            };
                                        specific.state.set((ProcState::Stopping, Utc::now()));
                                        on_stopping(&state_dynamic, &task_id);
                                    }

                                    // Signal stop
                                    gentle_stop_proc(pid, child, logger, spec.stop_timeout).await?;

                                    // Mark as stopped
                                    {
                                        let mut state_dynamic = state.dynamic.lock().unwrap();
                                        let TaskStateSpecific::Short(specific) =
                                            &get_task(&state_dynamic, &task_id).specific else {
                                                panic!();
                                            };
                                        specific.state.set((ProcState::Stopped, Utc::now()));
                                        specific.pid.set(None);
                                        on_stopped(&state_dynamic, &task_id);
                                        if let Some(started_action) = &specific.spec.started_action {
                                            match started_action {
                                                interface::task::ShortTaskStartedAction::None => { },
                                                interface::task::ShortTaskStartedAction::TurnOff => { },
                                                interface::task::ShortTaskStartedAction::Delete => {
                                                    delete_task(&mut state_dynamic, &task_id);
                                                },
                                            }
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
                                warn!(err = e.to_string(), "Long process failed with error");
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
                                    on_stopped(&state_dynamic, &task_id);
                                    if let Some(started_action) = &specific.spec.started_action {
                                        match started_action {
                                            interface::task::ShortTaskStartedAction::None => { },
                                            interface::task::ShortTaskStartedAction::TurnOff => { },
                                            interface::task::ShortTaskStartedAction::Delete => {
                                                delete_task(&mut state_dynamic, &task_id);
                                            },
                                        }
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }.instrument(info_span!("task_short", task_id = task.id)));
            return false;
        },
        TaskStateSpecific::External => unreachable!(),
    }
}

pub(crate) fn set_task_user_on(state: &Arc<State>, state_dynamic: &StateDynamic, root_task_id: &TaskId) {
    // Update on flags and check if the effective `on` state has changed
    {
        let task = get_task(state_dynamic, root_task_id);
        let was_on = task_on(&task);
        task.user_on.set((true, Utc::now()));
        if was_on {
            return;
        }

        // Set transitive_on for strong deps, start leaves
        {
            let mut frontier = vec![];

            fn push_frontier(frontier: &mut Vec<(bool, TaskId)>, task: &TaskState_) {
                upstream(&task, |upstream| {
                    for (upstream_id, upstream_type) in upstream {
                        match upstream_type {
                            DependencyType::Strong => { },
                            DependencyType::Weak => {
                                continue;
                            },
                        }
                        frontier.push((true, upstream_id.clone()));
                    }
                });
            }

            push_frontier(&mut frontier, get_task(state_dynamic, &root_task_id));
            while let Some((first, upstream_id)) = frontier.pop() {
                if first {
                    let upstream_task = get_task(state_dynamic, &upstream_id);
                    let was_on = task_on(&upstream_task);
                    upstream_task.transitive_on.set((true, Utc::now()));
                    if was_on {
                        continue;
                    }
                    frontier.push((false, upstream_id));
                    push_frontier(&mut frontier, upstream_task);
                } else {
                    let upstream_task = get_task(state_dynamic, &upstream_id);
                    if all_upstream_tasks_started(state_dynamic, &upstream_task) {
                        do_start_task(state, state_dynamic, &upstream_task);
                    }
                }
            }
        }

        // Start this
        if !all_upstream_tasks_started(state_dynamic, task) {
            return;
        }
        if !do_start_task(state, state_dynamic, task) {
            return;
        }
    }

    // If everything else has started, start downstream
    propagate_start_downstream(state, state_dynamic, root_task_id);
}

pub(crate) fn can_stop_task(task: &TaskState_) -> bool {
    if task_stopped(task) {
        return false;
    }
    match &task.specific {
        TaskStateSpecific::Empty(_) => {
            return true;
        },
        TaskStateSpecific::Long(specific) => {
            return specific.stop.borrow().is_some();
        },
        TaskStateSpecific::Short(specific) => {
            return specific.stop.borrow().is_some();
        },
        TaskStateSpecific::External => unreachable!(),
    }
}

/// Return true if task is finished stopping (can continue with upstream).
pub(crate) fn do_stop_task(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    if !all_downstream_tasks_stopped(state_dynamic, &task) {
        return false;
    }
    if task_stopped(task) {
        return true;
    }
    match &task.specific {
        TaskStateSpecific::Empty(specific) => {
            log_stopping(&task.id);
            specific.started.set((false, Utc::now()));
            on_stopped(state_dynamic, &task.id);
            return true;
        },
        TaskStateSpecific::Long(specific) => {
            if let Some(stop) = specific.stop.take() {
                _ = stop.send(());
            }
            return false;
        },
        TaskStateSpecific::Short(specific) => {
            if let Some(stop) = specific.stop.take() {
                _ = stop.send(());
            }
            return false;
        },
        TaskStateSpecific::External => unreachable!(),
    }
}

pub(crate) fn propagate_transitive_off(state_dynamic: &StateDynamic, task_id: &TaskId) {
    let mut frontier = vec![];

    fn push_upstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
        upstream(&task, |upstream| {
            for (up_id, up_dep_type) in upstream {
                match up_dep_type {
                    DependencyType::Strong => { },
                    DependencyType::Weak => {
                        // Hadn't started, so shouldn't stop
                        continue;
                    },
                }
                frontier.push(up_id.clone());
            }
        });
    }

    push_upstream(&mut frontier, get_task(&state_dynamic, task_id));
    while let Some(upstream_id) = frontier.pop() {
        let upstream_task = get_task(state_dynamic, &upstream_id);
        if !task_on(&upstream_task) {
            // Subtree already done, skip
            continue;
        }
        let mut all_downstream_off = true;
        for (downstream_id, downstream_type) in upstream_task.downstream.borrow().iter() {
            match *downstream_type {
                DependencyType::Strong => { },
                DependencyType::Weak => {
                    // Doesn't affect this task
                    continue;
                },
            }
            if task_on(get_task(state_dynamic, downstream_id)) {
                all_downstream_off = false;
                break;
            }
        }
        if !all_downstream_off {
            // Can't do anything
            continue;
        }

        // Not yet off, and all downstream off - confirmed this should be transitive off
        // now
        upstream_task.transitive_on.set((false, Utc::now()));

        // Recurse
        push_upstream(&mut frontier, upstream_task);
    }
}

pub(crate) fn set_task_user_off(state_dynamic: &StateDynamic, task_id: &TaskId) {
    // Update on flags and check if the effective `on` state has changed
    {
        let task = get_task(state_dynamic, &task_id);
        let was_off = !task_on(&task);
        if was_off {
            return;
        }
        task.user_on.set((false, Utc::now()));
        if task.transitive_on.get().0 {
            return;
        }
    }

    // Unset transitive_on for strong upstream deps
    propagate_transitive_off(state_dynamic, task_id);

    // Stop weak downstream tasks starting from leaves to current task
    let stopped;
    {
        let mut frontier = vec![(true, task_id.clone())];
        let mut all_downstream_stopped_stack = vec![true];
        while let Some((first_pass, downstream_id)) = frontier.pop() {
            if first_pass {
                let downstream_task = get_task(state_dynamic, &downstream_id);
                if !can_stop_task(&downstream_task) {
                    // Already stopping, nothing to do
                    continue;
                }
                frontier.push((false, downstream_id.clone()));

                // Descend
                all_downstream_stopped_stack.push(true);
                for (k, v) in downstream_task.downstream.borrow().iter() {
                    match *v {
                        DependencyType::Strong => {
                            // Must already be off for this to be transitively off
                            continue;
                        },
                        DependencyType::Weak => { },
                    }
                    frontier.push((true, k.clone()));
                }
            } else {
                // Stop if possible
                let downstream_task = get_task(state_dynamic, &downstream_id);
                let all_downstream_stopped = all_downstream_stopped_stack.pop().unwrap();
                let parent_all_downstream_stopped = all_downstream_stopped_stack.last_mut().unwrap();
                if all_downstream_stopped {
                    if !do_stop_task(state_dynamic, &downstream_task) {
                        *parent_all_downstream_stopped = false;
                    }
                } else {
                    *parent_all_downstream_stopped = false;
                }
            }
        }
        stopped = all_downstream_stopped_stack.pop().unwrap();
    }

    // Stop upstream if this is already stopped
    if stopped {
        propagate_stop_upstream(state_dynamic, task_id);
    }
}

// When a task starts, start the next dependent downstream tasks
pub(crate) fn propagate_start_downstream(state: &Arc<State>, state_dynamic: &StateDynamic, from_task_id: &TaskId) {
    let mut frontier = vec![];

    fn push_downstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
        frontier.extend(task.downstream.borrow().keys().cloned());
    }

    push_downstream(&mut frontier, get_task(state_dynamic, from_task_id));
    while let Some(downstream_id) = frontier.pop() {
        let downstream = get_task(state_dynamic, &downstream_id);
        if !task_on(&downstream) {
            continue;
        }
        if !do_start_task(state, state_dynamic, &downstream) {
            continue;
        }
        push_downstream(&mut frontier, downstream);
    }
}

// When a task stops, stop the next upstream tasks that were started as
// dependencies
pub(crate) fn propagate_stop_upstream(state_dynamic: &StateDynamic, task_id: &TaskId) {
    let mut frontier = vec![];

    fn push_upstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
        upstream(task, |upstream| {
            for (up_id, up_dep_type) in upstream {
                match up_dep_type {
                    DependencyType::Strong => { },
                    DependencyType::Weak => {
                        continue;
                    },
                }
                frontier.push(up_id.clone());
            }
        });
    }

    push_upstream(&mut frontier, get_task(state_dynamic, task_id));
    while let Some(upstream_id) = frontier.pop() {
        let upstream_task = get_task(state_dynamic, &upstream_id);
        if task_on(upstream_task) {
            continue;
        }
        if !do_stop_task(state_dynamic, &upstream_task) {
            continue;
        }
        push_upstream(&mut frontier, &upstream_task);
    }
}
