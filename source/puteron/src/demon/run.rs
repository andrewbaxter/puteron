use {
    super::{
        state::{
            State,
            StateDynamic,
            TaskStateSpecific,
        },
        task_create_delete::{
            build_task,
            validate_new_task,
        },
        task_util::upstream,
    },
    crate::{
        demon::{
            schedule::{
                self,
                pop_schedule,
                populate_schedule,
            },
            task_create_delete::delete_task,
            task_state::{
                set_task_user_off,
                set_task_user_on,
                task_on,
                task_started,
                task_stopped,
            },
            task_util::{
                get_task,
                maybe_get_task,
            },
        },
        ipc,
        spec::merge_specs,
    },
    aargvark::{
        traits_impls::AargvarkJson,
        Aargvark,
    },
    chrono::Utc,
    flowcontrol::ta_return,
    loga::{
        ea,
        DebugDisplay,
        ResultContext,
    },
    puteron_lib::interface::{
        self,
        message::v1::{
            RequestTrait,
            TaskDependencyStatus,
            TaskStatus,
        },
        task::{
            DependencyType,
            Task,
        },
        demon::Config,
    },
    std::{
        collections::{
            HashMap,
        },
        env,
        future::Future,
        sync::{
            Arc,
            Mutex,
        },
    },
    tokio::{
        fs::remove_file,
        net::{
            UnixListener,
            UnixStream,
        },
        runtime,
        select,
        signal::unix::SignalKind,
        spawn,
        sync::{
            oneshot,
            Notify,
        },
        time::{
            sleep_until,
            Instant,
        },
    },
    tracing::{
        debug,
        instrument,
    },
};

#[derive(Aargvark)]
pub struct DemonRunArgs {
    config: AargvarkJson<Config>,
}

pub(crate) fn main(args: DemonRunArgs) -> Result<(), loga::Error> {
    let config = args.config.value;
    let mut specs = merge_specs(&config.task_dirs, None)?;

    // # Prep env
    let mut env = HashMap::new();
    for (k, v) in env::vars() {
        if config.environment.keep_all || config.environment.keep.get(&k).cloned().unwrap_or(false) {
            env.insert(k, v);
        }
    }
    env.extend(config.environment.add);

    // # Create state
    let notify_reschedule = Arc::new(Notify::new());
    let state = Arc::new(State {
        task_dirs: config.task_dirs,
        env: env,
        dynamic: Mutex::new(StateDynamic {
            task_alloc: Default::default(),
            tasks: Default::default(),
            schedule: Default::default(),
            notify_reschedule: notify_reschedule.clone(),
        }),
        tokio_tasks: Default::default(),
    });
    {
        let mut state_dynamic = state.dynamic.lock().unwrap();

        // # Create task states from specs
        let mut errors = vec![];
        while !specs.is_empty() {
            let mut did_work = false;
            let task_ids = specs.keys().cloned().collect::<Vec<_>>();
            for task_id in &task_ids {
                // Find frontier tasks (all upstreams created)
                let upstream = match &specs.get(task_id).unwrap() {
                    Task::Empty(s) => {
                        s.upstream.keys().collect()
                    },
                    Task::Long(s) => {
                        s.upstream.keys().collect()
                    },
                    Task::Short(s) => {
                        s.upstream.keys().collect()
                    },
                    Task::External => vec![],
                };
                let mut all_upstream_created = true;
                for upstream_id in upstream {
                    if state_dynamic.tasks.contains_key(upstream_id) {
                        // created, ok
                    } else if specs.contains_key(upstream_id) {
                        // not yet created
                        all_upstream_created = false;
                    } else {
                        // missing, pretend ok - missing will be logged later when validating
                    }
                }
                if !all_upstream_created {
                    continue;
                }

                // All deps created, now create this task
                did_work = true;
                let spec = specs.remove(task_id).unwrap();
                validate_new_task(&state_dynamic, &mut errors, task_id, &spec);
                build_task(&mut state_dynamic, task_id.clone(), spec);
            }
            if !did_work {
                errors.push(
                    loga::err_with(
                        "One or more tasks have cycles in their dependencies",
                        ea!(tasks = task_ids.dbg_str()),
                    ),
                );
                break;
            }
        }
        if !errors.is_empty() {
            return Err(loga::agg_err("One or more errors with task specifications", errors));
        }
    }

    // # Start async
    let rt = runtime::Builder::new_multi_thread().enable_all().build().context("Error starting async runtime")?;
    rt.block_on(async move {
        ta_return!((), loga::Error);
        let mut schedule_delay;
        let mut schedule_next;
        {
            let mut state_dynamic = state.dynamic.lock().unwrap();

            // ## Start default-on tasks
            for (id, task) in &state_dynamic.tasks {
                let task = &state_dynamic.task_alloc[*task];
                let user_on;
                match &task.specific {
                    TaskStateSpecific::Empty(s) => {
                        user_on = s.spec.default_on;
                    },
                    TaskStateSpecific::Long(s) => {
                        user_on = s.spec.default_on;
                    },
                    TaskStateSpecific::Short(s) => {
                        user_on = s.spec.default_on;
                    },
                    TaskStateSpecific::External => {
                        user_on = false;
                    },
                }
                debug!(task = task.id, on = user_on, "Task initial state");
                if !user_on {
                    continue;
                }
                set_task_user_on(&state, &state_dynamic, id);
            }

            // ## Schedule tasks
            populate_schedule(&mut state_dynamic);

            // Get initially scheduled task
            (schedule_delay, schedule_next) = pop_schedule(&mut state_dynamic);
        }

        // ## Handle ipc + other inputs (signals)
        let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt()).context("Error hooking into SIGINT")?;
        let mut sigterm =
            tokio::signal::unix::signal(SignalKind::terminate()).context("Error hooking into SIGTERM")?;
        let state = state.clone();

        fn task_off_all(state: &Arc<State>) {
            let state_dynamic = state.dynamic.lock().unwrap();
            for task_id in state_dynamic.tasks.keys() {
                set_task_user_off(&state_dynamic, task_id);
            }
        }

        let message_socket;
        if let Some(ipc_path) = ipc::ipc_path() {
            match remove_file(&ipc_path).await {
                Ok(_) => { },
                Err(e) => match e.kind() {
                    std::io::ErrorKind::NotFound => { },
                    _ => {
                        return Err(
                            e,
                        ).context_with("Error cleaning up old ipc socket", ea!(path = ipc_path.dbg_str()));
                    },
                },
            }
            message_socket = Some(UnixListener::bind(ipc_path).context("Error creating control socket")?);
        } else {
            message_socket = None;
        }
        eprintln!("sleep until schedule delay: {:?}", schedule_delay.duration_since(Instant::now()));
        let mut sigint = Box::pin(sigint.recv());
        let mut sigterm = Box::pin(sigterm.recv());
        loop {
            select!{
                _ =& mut sigint => {
                    debug!("Got sigint, shutting down.");
                    task_off_all(&state);
                    break;
                },
                _ =& mut sigterm => {
                    debug!("Got sigterm, shutting down.");
                    task_off_all(&state);
                    break;
                }
                accepted = message_socket.as_ref().unwrap().accept(),
                if message_socket.is_some() => {
                    let (stream, peer) = match accepted {
                        Ok((stream, peer)) => (stream, peer),
                        Err(e) => {
                            debug!(err = e.to_string(), "Error accepting connection");
                            continue;
                        },
                    };
                    spawn(handle_ipc(state.clone(), peer, stream));
                },
                _ = notify_reschedule.notified() => {
                    let mut state_dynamic = state.dynamic.lock().unwrap();
                    state_dynamic.schedule.entry(schedule_delay).or_default().push(schedule_next);
                    (schedule_delay, schedule_next) = pop_schedule(&mut state_dynamic);
                },
                _ = sleep_until(schedule_delay) => {
                    let mut state_dynamic = state.dynamic.lock().unwrap();
                    debug!(task = schedule_next.0, "Timer triggered for scheduled task, turning on");
                    set_task_user_on(&state, &mut state_dynamic, &schedule_next.0);
                    state_dynamic
                        .schedule
                        .entry(schedule::calc_next_instant(Utc::now(), Instant::now(), &schedule_next.1, false))
                        .or_default()
                        .push(schedule_next);
                    (schedule_delay, schedule_next) = schedule::pop_schedule(&mut state_dynamic);
                }
            }
        }

        // Waits for all tasks
        state.tokio_tasks.close();
        state.tokio_tasks.wait().await;
        return Ok(());
    })?;
    return Ok(());
}

#[instrument(skip_all, fields(peer =? peer))]
async fn handle_ipc(state: Arc<State>, peer: tokio::net::unix::SocketAddr, mut conn: UnixStream) {
    loop {
        let message = match ipc::read::<interface::message::Request>(&mut conn).await {
            Ok(Some(message)) => message,
            Ok(None) => {
                return;
            },
            Err(e) => {
                debug!(peer =? peer, error =? e, "Error reading message from connection");
                return;
            },
        };
        match {
            let state = state.clone();
            async move {
                ta_return!(Vec < u8 >, loga::Error);

                async fn handle<
                    I: RequestTrait,
                    F: Future<Output = I::Response>,
                >(req: I, cb: impl FnOnce(I) -> F) -> Result<Vec<u8>, loga::Error> {
                    return Ok(serde_json::to_vec(&cb(req).await).unwrap());
                }

                match message {
                    interface::message::Request::V1(m) => match m {
                        interface::message::v1::Request::TaskAdd(m) => return handle(m, |m| async move {
                            let mut state_dynamic = state.dynamic.lock().unwrap();

                            // # Check + delete the old task if it exists
                            if let Some(task) = maybe_get_task(&state_dynamic, &m.task) {
                                if !m.unique {
                                    return Err(format!("A task with this ID already exists"));
                                }
                                if !task_stopped(task) {
                                    return Err(format!("Task isn't stopped yet"));
                                }
                                let same = match (&m.spec, &task.specific) {
                                    (Task::Empty(new), TaskStateSpecific::Empty(old)) => new == &old.spec,
                                    (Task::Long(new), TaskStateSpecific::Long(old)) => new == &old.spec,
                                    (Task::Short(new), TaskStateSpecific::Short(old)) => new == &old.spec,
                                    (Task::External, TaskStateSpecific::External) => true,
                                    _ => false,
                                };
                                if same {
                                    return Ok(());
                                }
                                delete_task(&mut state_dynamic, &m.task);
                            }

                            // # Check new task spec
                            //
                            // Check for broken upstreams
                            let mut errors = vec![];
                            validate_new_task(&state_dynamic, &mut errors, &m.task, &m.spec);
                            if !errors.is_empty() {
                                return Err(
                                    format!(
                                        "Task has errors:\n{}",
                                        errors
                                            .into_iter()
                                            .map(|x| format!("- {}", x))
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    ),
                                );
                            }

                            // # Create task
                            let user_on = match &m.spec {
                                Task::Empty(s) => s.default_on,
                                Task::Long(s) => s.default_on,
                                Task::Short(s) => s.default_on,
                                Task::External => false,
                            };
                            build_task(&mut state_dynamic, m.task.clone(), m.spec);

                            // # Turn on maybe
                            if user_on {
                                set_task_user_on(&state, &mut state_dynamic, &m.task);
                            }
                            return Ok(());
                        }).await,
                        interface::message::v1::Request::TaskDelete(m) => return handle(m, |m| async move {
                            let mut state_dynamic = state.dynamic.lock().unwrap();
                            let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                                return Ok(());
                            };
                            if !task_stopped(&task) {
                                return Err(format!("Task isn't stopped yet"));
                            }
                            delete_task(&mut state_dynamic, &m.0);
                            return Ok(());
                        }).await,
                        interface::message::v1::Request::TaskGetStatus(m) => return handle(m, |m| async move {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                                return Err(format!("Unknown task [{}]", m.0));
                            };
                            return Ok(TaskStatus {
                                direct_on: task.user_on.get().0,
                                direct_on_at: task.user_on.get().1,
                                transitive_on: task.transitive_on.get().0,
                                transitive_on_at: task.transitive_on.get().1,
                                specific: match &task.specific {
                                    TaskStateSpecific::Empty(s) => interface::message::v1::TaskStatusSpecific::Empty(
                                        interface::message::v1::TaskStatusSpecificEmpty {
                                            started: s.started.get().0,
                                            started_at: s.started.get().1,
                                        },
                                    ),
                                    TaskStateSpecific::Long(s) => interface::message::v1::TaskStatusSpecific::Long(
                                        interface::message::v1::TaskStatusSpecificLong {
                                            state: s.state.get().0,
                                            state_at: s.state.get().1,
                                            pid: s.pid.get(),
                                            restarts: s.failed_start_count.get(),
                                        },
                                    ),
                                    TaskStateSpecific::Short(s) => interface::message::v1::TaskStatusSpecific::Short(
                                        interface::message::v1::TaskStatusSpecificShort {
                                            state: s.state.get().0,
                                            state_at: s.state.get().1,
                                            pid: s.pid.get(),
                                            restarts: s.failed_start_count.get(),
                                        },
                                    ),
                                    TaskStateSpecific::External => interface
                                    ::message
                                    ::v1
                                    ::TaskStatusSpecific
                                    ::External,
                                },
                            });
                        }).await,
                        interface::message::v1::Request::TaskGetSpec(m) => return handle(m, |m| async move {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                                return Err(format!("Unknown task [{}]", m.0));
                            };
                            let out;
                            match &task.specific {
                                TaskStateSpecific::Empty(s) => {
                                    out = Task::Empty(s.spec.clone());
                                },
                                TaskStateSpecific::Long(s) => {
                                    out = Task::Long(s.spec.clone());
                                },
                                TaskStateSpecific::Short(s) => {
                                    out = Task::Short(s.spec.clone());
                                },
                                TaskStateSpecific::External => {
                                    out = Task::External;
                                },
                            }
                            return Ok(out);
                        }).await,
                        interface::message::v1::Request::TaskOn(m) => return handle(m, |m| async move {
                            let mut state_dynamic = state.dynamic.lock().unwrap();
                            if !state_dynamic.tasks.contains_key(&m.task) {
                                return Err(format!("Unknown task [{}]", m.task));
                            }
                            if m.on {
                                set_task_user_on(&state, &mut state_dynamic, &m.task);
                                return Ok(());
                            } else {
                                set_task_user_off(&mut state_dynamic, &m.task);
                                return Ok(());
                            }
                        }).await,
                        interface::message::v1::Request::TaskWaitStarted(m) => return handle(m, |m| async move {
                            let (notify_tx, notify_rx) = oneshot::channel();
                            {
                                let state_dynamic = state.dynamic.lock().unwrap();
                                let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                                    return Err(format!("Unknown task [{}]", m.0));
                                };
                                if task_started(task) {
                                    return Ok(());
                                }
                                task.started_waiters.borrow_mut().push(notify_tx);
                            }
                            match notify_rx.await {
                                Ok(res) => {
                                    if res {
                                        return Ok(());
                                    } else {
                                        return Err("Start canceled; task is now stopping".to_string());
                                    }
                                },
                                Err(e) => {
                                    return Err(e.to_string());
                                },
                            }
                        }).await,
                        interface::message::v1::Request::TaskWaitStopped(m) => return handle(m, |m| async move {
                            let (notify_tx, notify_rx) = oneshot::channel();
                            {
                                let state_dynamic = state.dynamic.lock().unwrap();
                                let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                                    return Err(format!("Unknown task [{}]", m.0));
                                };
                                if task_stopped(task) {
                                    return Ok(());
                                }
                                task.stopped_waiters.borrow_mut().push(notify_tx);
                            }
                            match notify_rx.await {
                                Ok(res) => {
                                    if res {
                                        return Ok(());
                                    } else {
                                        return Err("Stop canceled; task is now starting".to_string());
                                    }
                                },
                                Err(e) => {
                                    return Err(e.to_string());
                                },
                            }
                        }).await,
                        interface::message::v1::Request::TaskListUserOn(m) => return handle(m, |_m| async move {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let mut out = vec![];
                            for (task_id, state) in &state_dynamic.tasks {
                                if state_dynamic.task_alloc[*state].user_on.get().0 {
                                    out.push(task_id.clone());
                                }
                            }
                            return Ok(out);
                        }).await,
                        interface::message::v1::Request::TaskListUpstream(m) => return handle(m, |m| async move {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            if !state_dynamic.tasks.contains_key(&m.0) {
                                return Err(format!("Unknown task [{}]", m.0));
                            }
                            let mut out_stack = vec![];
                            let mut root = None;
                            let mut frontier = vec![(true, m.0.clone(), DependencyType::Strong)];
                            while let Some((first, task_id, dependency_type)) = frontier.pop() {
                                if first {
                                    frontier.push((false, task_id.clone(), dependency_type));
                                    let push_status;
                                    let task = get_task(&state_dynamic, &task_id);
                                    push_status = TaskDependencyStatus {
                                        on: task_on(task),
                                        started: task_started(task),
                                        dependency_type: dependency_type,
                                        related: HashMap::new(),
                                    };
                                    upstream(task, |upstream| {
                                        for (next_id, next_dep_type) in upstream {
                                            frontier.push((true, next_id.clone(), match dependency_type {
                                                DependencyType::Strong => *next_dep_type,
                                                DependencyType::Weak => DependencyType::Weak,
                                            }));
                                        }
                                    });
                                    out_stack.push((task_id, push_status));
                                } else {
                                    let (top_id, top) = out_stack.pop().unwrap();
                                    if let Some(parent) = out_stack.last_mut() {
                                        parent.1.related.insert(top_id, top);
                                    } else {
                                        root = Some(top.related);
                                    }
                                }
                            }
                            return Ok(root.unwrap());
                        }).await,
                        interface::message::v1::Request::TaskListDownstream(m) => return handle(m, |m| async move {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            if !state_dynamic.tasks.contains_key(&m.0) {
                                return Err(format!("Unknown task [{}]", m.0));
                            }
                            let mut out_stack = vec![];
                            let mut root = None;
                            let mut frontier = vec![(true, m.0.clone(), DependencyType::Strong)];
                            while let Some((first, task_id, dependency_type)) = frontier.pop() {
                                if first {
                                    frontier.push((false, task_id.clone(), dependency_type));
                                    let push_status;
                                    let task = get_task(&state_dynamic, &task_id);
                                    push_status = TaskDependencyStatus {
                                        on: task_on(task),
                                        started: task_started(task),
                                        dependency_type: dependency_type,
                                        related: HashMap::new(),
                                    };
                                    for (down_id, down_type) in task.downstream.borrow().iter() {
                                        frontier.push((true, down_id.clone(), *down_type));
                                    }
                                    out_stack.push((task_id, push_status));
                                } else {
                                    let (top_id, top) = out_stack.pop().unwrap();
                                    if let Some(parent) = out_stack.last_mut() {
                                        parent.1.related.insert(top_id, top);
                                    } else {
                                        root = Some(top.related);
                                    }
                                }
                            }
                            return Ok(root.unwrap());
                        }).await,
                        interface::message::v1::Request::DemonEnv(m) => return handle(m, |_m| async {
                            return Ok(state.env.clone());
                        }).await,
                        interface::message::v1::Request::DemonSpecDirs(m) => return handle(m, |_m| async {
                            return Ok(state.task_dirs.clone());
                        }).await,
                    },
                }
            }
        }.await {
            Ok(body) => {
                match ipc::write(&mut conn, &body).await {
                    Ok(_) => { },
                    Err(e) => {
                        debug!(err = e.to_string(), "Error writing response");
                    },
                }
            },
            Err(e) => {
                debug!(err = e.to_string(), "Error handling message");
            },
        }
    }
}
