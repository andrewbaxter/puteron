use {
    super::state::{
        all_upstream_tasks_started,
        task_on,
        task_started,
        task_stopped,
        upstream,
        State,
        StateDynamic,
        TaskStateEmpty,
        TaskStateFinite,
        TaskStatePerpetual,
        TaskStateSpecific,
        TaskState_,
    },
    crate::{
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
    puteron_lib::{
        duration::SimpleDuration,
        interface::{
            self,
            base::TaskId,
            message::v1::{
                ProcState,
                RequestTrait,
                TaskDownstreamStatus,
                TaskStatus,
                TaskUpstreamStatus,
            },
            task::{
                DependencyType,
                Task,
            },
        },
    },
    rustix::{
        process::Signal,
        termios::Pid,
    },
    serde::{
        Deserialize,
        Serialize,
    },
    std::{
        cell::{
            Cell,
            RefCell,
        },
        collections::{
            HashMap,
            HashSet,
        },
        env,
        future::Future,
        path::PathBuf,
        pin::Pin,
        process::Stdio,
        sync::{
            Arc,
            Mutex,
        },
        time::Duration,
    },
    syslog::Formatter3164,
    tokio::{
        fs::remove_file,
        io::{
            AsyncBufReadExt,
            BufReader,
        },
        net::{
            TcpStream,
            UnixListener,
            UnixStream,
        },
        process::{
            Child,
            Command,
        },
        runtime,
        select,
        signal::unix::SignalKind,
        spawn,
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
        instrument,
        warn,
        Instrument,
    },
};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Config {
    #[serde(default)]
    environment: interface::task::Environment,
    #[serde(default)]
    task_dirs: Vec<PathBuf>,
}

#[derive(Aargvark)]
pub(crate) struct DemonRunArgs {
    config: AargvarkJson<Config>,
}

pub(crate) fn main(args: DemonRunArgs) -> Result<(), loga::Error> {
    let config = args.config.value;
    let specs = merge_specs(&config.task_dirs, None)?;

    // # Prep env
    let mut env = HashMap::new();
    match config.environment.clear {
        Some(keep) => {
            for (k, ok) in keep {
                if !ok {
                    continue;
                }
                match env::var(&k) {
                    Ok(v) => {
                        env.insert(k, v);
                    },
                    Err(e) => {
                        warn!(key = k, err = e.to_string(), "Failed to read env var, treating as unset");
                        continue;
                    },
                }
            }
        },
        None => {
            env.extend(env::vars());
        },
    }
    env.extend(config.environment.add);

    // # Create state
    let state = Arc::new(State {
        task_dirs: config.task_dirs,
        env: env,
        dynamic: Mutex::new(StateDynamic {
            task_alloc: Default::default(),
            tasks: Default::default(),
        }),
    });
    {
        let mut state_dynamic = state.dynamic.lock().unwrap();

        // # Create task states from specs
        let mut dependents = HashMap::<TaskId, HashMap<TaskId, DependencyType>>::new();
        for (id, spec) in specs {
            let task = state_dynamic.task_alloc.insert(build_task(&mut dependents, id.clone(), spec));
            state_dynamic.tasks.insert(id, task);
        }
        for (task_id, task) in state_dynamic.tasks.iter() {
            let task = &state_dynamic.task_alloc[*task];
            if let Some(dependents) = dependents.remove(task_id) {
                *task.downstream.borrow_mut() = dependents;
            }
        }

        // Check for cycles
        {
            let mut cycle_free = HashSet::new();
            for (task_id, task) in &state_dynamic.tasks {
                // Only check leaves, then go via upstreams
                {
                    let task = &state_dynamic.task_alloc[*task];
                    if !task.downstream.borrow().is_empty() {
                        continue;
                    }
                }
                if let Some(cycle) = task_find_cycles(&state_dynamic, &mut cycle_free, task_id) {
                    return Err(loga::err_with("Task cycle detected", ea!(cycle = cycle.dbg_str())));
                }
            }
        }
    }

    // # Start async
    let rt = runtime::Builder::new_multi_thread().enable_all().build().context("Error starting async runtime")?;
    rt.block_on(async move {
        ta_return!((), loga::Error);

        // ## Start default-on tasks
        {
            let state_dynamic = state.dynamic.lock().unwrap();
            for (id, task) in &state_dynamic.tasks {
                let task = &state_dynamic.task_alloc[*task];
                let user_on;
                match &task.specific {
                    TaskStateSpecific::Empty(s) => {
                        user_on = s.spec.default_on;
                    },
                    TaskStateSpecific::Perpetual(s) => {
                        user_on = s.spec.default_on;
                    },
                    TaskStateSpecific::Finite(s) => {
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
        }

        // ## Handle ipc + other inputs (signals)
        let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt()).context("Error hooking into SIGINT")?;
        let mut sigterm =
            tokio::signal::unix::signal(SignalKind::terminate()).context("Error hooking into SIGTERM")?;
        let state = state.clone();
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
            let message_socket = UnixListener::bind(ipc_path).context("Error creating control socket")?;
            let mut sigint = Box::pin(sigint.recv());
            let mut sigterm = Box::pin(sigterm.recv());
            loop {
                fn task_off_all(state: &Arc<State>) {
                    let state_dynamic = state.dynamic.lock().unwrap();
                    for task_id in state_dynamic.tasks.keys() {
                        set_task_user_off(&state_dynamic, task_id);
                    }
                }

                select!{
                    _ =& mut sigint => {
                        task_off_all(&state);
                        break;
                    },
                    _ =& mut sigterm => {
                        task_off_all(&state);
                        break;
                    }
                    accepted = message_socket.accept() => {
                        let (stream, peer) = match accepted {
                            Ok((stream, peer)) => (stream, peer),
                            Err(e) => {
                                debug!(err = e.to_string(), "Error accepting connection");
                                continue;
                            },
                        };
                        spawn(handle_ipc(state.clone(), peer, stream));
                    }
                }
            }
        } else {
            let mut sigint = Box::pin(sigint.recv());
            let mut sigterm = Box::pin(sigterm.recv());
            loop {
                select!{
                    _ =& mut sigint => {
                        break;
                    },
                    _ =& mut sigterm => {
                        break;
                    }
                }
            }
        }
        return Ok(());
    })?;

    // Waits for all tasks
    drop(rt);
    return Ok(());
}

fn task_find_cycles(
    state_dynamic: &StateDynamic,
    cycle_free: &mut HashSet<TaskId>,
    task_id: &TaskId,
) -> Option<Vec<TaskId>> {
    let mut frontier = vec![(true, task_id.clone())];
    let mut path: Vec<String> = vec![];
    while let Some((first, task_id)) = frontier.pop() {
        if first {
            if cycle_free.contains(&task_id) {
                continue;
            }
            if let Some(offset) = path.iter().enumerate().find_map(|(index, path_id)| {
                if path_id == &task_id {
                    return Some(index);
                } else {
                    return None;
                }
            }) {
                let mut cycle = (&path[offset..]).to_vec();
                cycle.push(task_id);
                return Some(cycle);
            }
            path.push(task_id.clone());
            frontier.push((false, task_id.clone()));
            upstream(&state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()], |upstream| {
                for (t, _) in upstream {
                    frontier.push((true, t.clone()));
                }
            });
        } else {
            path.pop();
            cycle_free.insert(task_id);
        }
    }
    return None;
}

fn delete_task(state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    {
        let task = state_dynamic.tasks.get(task_id).unwrap();
        let task = &state_dynamic.task_alloc[*task];
        upstream(task, |upstream| {
            for (dep_id, _) in upstream {
                let Some(dep) = state_dynamic.tasks.get(dep_id) else {
                    continue;
                };
                let dep = &state_dynamic.task_alloc[*dep];
                dep.downstream.borrow_mut().remove(task_id);
            }
        });
    }
    state_dynamic.tasks.remove(task_id);
}

fn build_task(
    dependents: &mut HashMap::<TaskId, HashMap<TaskId, DependencyType>>,
    task_id: TaskId,
    spec: Task,
) -> TaskState_ {
    let specific;
    match spec {
        interface::task::Task::Empty(spec) => {
            for (dependency_id, dependency_info) in &spec.upstream {
                let task_dependents = dependents.entry(dependency_id.clone()).or_default();
                task_dependents.insert(task_id.clone(), dependency_info.clone());
            }
            specific = TaskStateSpecific::Empty(TaskStateEmpty {
                started: Cell::new((false, Utc::now())),
                spec: spec,
            });
        },
        interface::task::Task::Perpetual(spec) => {
            for (dependency_id, dependency_info) in &spec.upstream {
                let task_dependents = dependents.entry(dependency_id.clone()).or_default();
                task_dependents.insert(task_id.clone(), dependency_info.clone());
            }
            specific = TaskStateSpecific::Perpetual(TaskStatePerpetual {
                spec: spec,
                state: Cell::new((ProcState::Stopped, Utc::now())),
                stop: RefCell::new(None),
                pid: Cell::new(None),
                failed_start_count: Cell::new(0),
            });
        },
        interface::task::Task::Finite(spec) => {
            for (dependency_id, dependency_info) in &spec.upstream {
                let task_dependents = dependents.entry(dependency_id.clone()).or_default();
                task_dependents.insert(task_id.clone(), dependency_info.clone());
            }
            specific = TaskStateSpecific::Finite(TaskStateFinite {
                spec: spec,
                state: Cell::new((ProcState::Stopped, Utc::now())),
                stop: RefCell::new(None),
                pid: Cell::new(None),
                failed_start_count: Cell::new(0),
            });
        },
        interface::task::Task::External => {
            specific = TaskStateSpecific::External;
        },
    }
    return TaskState_ {
        id: task_id.clone(),
        user_on: Cell::new((false, Utc::now())),
        transitive_on: Cell::new((false, Utc::now())),
        downstream: RefCell::new(HashMap::new()),
        specific: specific,
        started_waiters: RefCell::new(Default::default()),
        stopped_waiters: RefCell::new(Default::default()),
    };
}

fn load_task(state_dynamic: &mut StateDynamic, task_id: TaskId, spec: Task) -> Result<(), loga::Error> {
    if let Some(cycle) = task_find_cycles(&state_dynamic, &mut Default::default(), &task_id) {
        return Err(loga::err_with("Task cycle detected", ea!(cycle = cycle.dbg_str())));
    }
    let mut downstream = Default::default();
    let task = state_dynamic.task_alloc.insert(build_task(&mut downstream, task_id.clone(), spec));
    state_dynamic.tasks.insert(task_id, task);
    for (task_id, task_downstream) in downstream {
        let task = state_dynamic.tasks.get(&task_id).unwrap();
        let task = &state_dynamic.task_alloc[*task];
        task.downstream.borrow_mut().extend(task_downstream);
    }
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
                            if let Some(task) = state_dynamic.tasks.get(&m.task) {
                                let task = &state_dynamic.task_alloc[*task];
                                if !m.unique {
                                    return Err(format!("A task with this ID already exists"));
                                }
                                if !task_stopped(task) {
                                    return Err(format!("Task isn't stopped yet"));
                                }
                                let same = match (&m.spec, &task.specific) {
                                    (Task::Empty(new), TaskStateSpecific::Empty(old)) => new == &old.spec,
                                    (Task::Perpetual(new), TaskStateSpecific::Perpetual(old)) => new == &old.spec,
                                    (Task::Finite(new), TaskStateSpecific::Finite(old)) => new == &old.spec,
                                    (Task::External, TaskStateSpecific::External) => true,
                                    _ => false,
                                };
                                if same {
                                    return Ok(());
                                }
                                delete_task(&mut state_dynamic, &m.task);
                            }
                            load_task(&mut state_dynamic, m.task, m.spec).map_err(|e| e.to_string())?;
                            return Ok(());
                        }).await,
                        interface::message::v1::Request::TaskDelete(m) => return handle(m, |m| async move {
                            let mut state_dynamic = state.dynamic.lock().unwrap();
                            let Some(task) = state_dynamic.tasks.get(&m.0) else {
                                return Ok(());
                            };
                            let task = &state_dynamic.task_alloc[*task];
                            if !task_stopped(&task) {
                                return Err(format!("Task isn't stopped yet"));
                            }
                            delete_task(&mut state_dynamic, &m.0);
                            return Ok(());
                        }).await,
                        interface::message::v1::Request::TaskGetStatus(m) => return handle(m, |m| async move {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let Some(task) = state_dynamic.tasks.get(&m.0) else {
                                return Err(format!("Unknown task [{}]", m.0));
                            };
                            let task = &state_dynamic.task_alloc[*task];
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
                                    TaskStateSpecific::Perpetual(s) => interface
                                    ::message
                                    ::v1
                                    ::TaskStatusSpecific
                                    ::Perpetual(
                                        interface::message::v1::TaskStatusSpecificPerpetual {
                                            state: s.state.get().0,
                                            state_at: s.state.get().1,
                                            pid: s.pid.get(),
                                            restarts: s.failed_start_count.get(),
                                        },
                                    ),
                                    TaskStateSpecific::Finite(s) => interface::message::v1::TaskStatusSpecific::Finite(
                                        interface::message::v1::TaskStatusSpecificFinite {
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
                            let Some(task) = state_dynamic.tasks.get(&m.0) else {
                                return Err(format!("Unknown task [{}]", m.0));
                            };
                            let task = &state_dynamic.task_alloc[*task];
                            let out;
                            match &task.specific {
                                TaskStateSpecific::Empty(s) => {
                                    out = Task::Empty(s.spec.clone());
                                },
                                TaskStateSpecific::Perpetual(s) => {
                                    out = Task::Perpetual(s.spec.clone());
                                },
                                TaskStateSpecific::Finite(s) => {
                                    out = Task::Finite(s.spec.clone());
                                },
                                TaskStateSpecific::External => {
                                    out = Task::External;
                                },
                            }
                            return Ok(out);
                        }).await,
                        interface::message::v1::Request::TaskOn(m) => return handle(m, |m| async move {
                            let mut state_dynamic = state.dynamic.lock().unwrap();
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
                                let Some(task) = state_dynamic.tasks.get(&m.0) else {
                                    return Err(format!("Unknown task [{}]", m.0));
                                };
                                let task = &state_dynamic.task_alloc[*task];
                                if !task_on(task) {
                                    return Err(format!("Task [{}] is not on", m.0));
                                }
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
                                        return Err("Task was turned off".to_string());
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
                                let Some(task) = state_dynamic.tasks.get(&m.0) else {
                                    return Err(format!("Unknown task [{}]", m.0));
                                };
                                let task = &state_dynamic.task_alloc[*task];
                                if task_on(task) {
                                    return Err(format!("Task [{}] is not off", m.0));
                                }
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
                                        return Err("Task was turned on".to_string());
                                    }
                                },
                                Err(e) => {
                                    return Err(e.to_string());
                                },
                            }
                        }).await,
                        interface::message::v1::Request::TaskShowUpstream(m) => return handle(m, |m| async move {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let mut out_stack = vec![];
                            let mut root = None;
                            let mut frontier = vec![(true, m.0.clone(), DependencyType::Strong)];
                            while let Some((first, task_id, dependency_type)) = frontier.pop() {
                                if first {
                                    frontier.push((false, task_id.clone(), dependency_type));
                                    if let Some(task) = state_dynamic.tasks.get(&task_id) {
                                        let task = &state_dynamic.task_alloc[*task];
                                        out_stack.push(Some(TaskUpstreamStatus {
                                            task: task_id,
                                            on: task_on(task),
                                            started: task_started(task),
                                            dependency_type: dependency_type,
                                            related: HashMap::new(),
                                        }));
                                        upstream(task, |upstream| {
                                            for (next_id, next_dep_type) in upstream {
                                                frontier.push((true, next_id.clone(), match dependency_type {
                                                    DependencyType::Strong => *next_dep_type,
                                                    DependencyType::Weak => DependencyType::Weak,
                                                }));
                                            }
                                        });
                                    } else {
                                        out_stack.push(None);
                                    }
                                } else {
                                    let top = out_stack.pop().unwrap();
                                    if let Some(parent) = out_stack.last_mut() {
                                        let parent = parent.as_mut().unwrap();
                                        parent.related.insert(task_id, top);
                                    } else {
                                        root = top.map(|x| x.related);
                                    }
                                }
                            }
                            let Some(root) = root else {
                                return Err(format!("Unknown task [{}]", m.0));
                            };
                            return Ok(root);
                        }).await,
                        interface::message::v1::Request::TaskShowDownstream(m) => return handle(m, |m| async move {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let mut out_stack = vec![];
                            let mut root = None;
                            let mut frontier = vec![(true, m.0.clone())];
                            while let Some((first, task_id)) = frontier.pop() {
                                if first {
                                    frontier.push((false, task_id.clone()));
                                    let push_status;
                                    if let Some(task) = state_dynamic.tasks.get(&task_id) {
                                        let task = &state_dynamic.task_alloc[*task];
                                        push_status = Some(TaskDownstreamStatus {
                                            task: task_id,
                                            on: task_on(task),
                                            started: task_started(task),
                                            related: HashMap::new(),
                                        });
                                        for (dep_id, _) in task.downstream.borrow().iter() {
                                            frontier.push((true, dep_id.clone()));
                                        }
                                    } else {
                                        push_status = None;
                                    }
                                    out_stack.push(push_status);
                                } else {
                                    let top = out_stack.pop().unwrap();
                                    if let Some(parent) = out_stack.last_mut() {
                                        let parent = parent.as_mut().unwrap();
                                        parent.related.insert(task_id, top);
                                    } else {
                                        root = top.map(|x| x.related);
                                    }
                                }
                            }
                            let Some(root) = root else {
                                return Err(format!("Unknown task [{}]", m.0));
                            };
                            return Ok(root);
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

fn all_downstream_tasks_stopped(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    for (task_id, _) in &*task.downstream.borrow() {
        let Some(dep) = state_dynamic.tasks.get(task_id) else {
            return false;
        };
        if !task_stopped(&state_dynamic.task_alloc[*dep]) {
            return false;
        }
    }
    return true;
}

/// Return true if started - downstream can be started now.
fn do_start_task(state: &Arc<State>, state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    if !all_upstream_tasks_started(&state_dynamic, task) {
        return false;
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
        base_env: &HashMap<String, String>,
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
        if let Some(clear_env) = &spec.environment.clear {
            command.env_clear();
            for (k, keep) in clear_env {
                if !keep {
                    continue;
                }
                if let Some(v) = base_env.get(k) {
                    command.env(k, v);
                }
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
        let mut child = command.spawn().context("Failed to spawn subprocess")?;
        drop(command);
        let pid = Pid::from_raw(child.id().unwrap() as i32).unwrap();

        // Stdout/err -> syslog 2
        let logger = Box::pin(spawn({
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
        let mut frontier = vec![];
        {
            let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(task_id).unwrap()];
            frontier.extend(task.downstream.borrow().keys().cloned());
        }

        // Stop all downstream immediately
        while let Some(task_id) = frontier.pop() {
            let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
            if task_started(&task) {
                do_stop_task(&task);
                frontier.extend(task.downstream.borrow().keys().cloned());
            }
        }
    }

    fn on_started(state: &Arc<State>, state_dynamic: &StateDynamic, task_id: &TaskId) {
        push_started(state, state_dynamic, task_id);
    }

    match &task.specific {
        TaskStateSpecific::Empty(s) => {
            debug!(task = task.id, "Starting task");
            s.started.set((true, Utc::now()));
            return true;
        },
        TaskStateSpecific::Perpetual(s) => {
            if s.state.get().0 != ProcState::Stopped {
                return false;
            }

            // Mark as starting
            s.state.set((ProcState::Starting, Utc::now()));

            // Start
            let (stop_tx, mut stop_rx) = oneshot::channel();
            *s.stop.borrow_mut() = Some(stop_tx);
            let spec = s.spec.clone();
            let task_id = task.id.clone();
            let state = state.clone();
            spawn(async move {
                let restart_delay = Duration::from(spec.restart_delay.unwrap_or(SimpleDuration {
                    count: 1,
                    unit: puteron_lib::duration::SimpleDurationUnit::Minute,
                }).into());
                loop {
                    debug!(task = task_id, "Starting task");
                    match async {
                        ta_return!(bool, loga::Error);
                        let (mut child, pid, logger) = spawn_proc(&state.env, &task_id, &spec.command)?;
                        {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                            let TaskStateSpecific::Perpetual(specific) = &task.specific else {
                                panic!();
                            };
                            specific.pid.set(Some(pid.as_raw_nonzero().get()));
                        }
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
                            debug!(task = task_id, "Confirmed task started");
                            {
                                let state_dynamic = state.dynamic.lock().unwrap();
                                let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                                let TaskStateSpecific::Perpetual(specific) = &task.specific else {
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
                        select!{
                            _ = live_work => {
                                unreachable!();
                            },
                            _ =& mut stop_rx => {
                                debug!(task = task_id, "Got stop signal, stopping");

                                // Mark as stopping + do state updates
                                {
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                                    let TaskStateSpecific::Perpetual(specific) = &task.specific else {
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
                                    let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                                    let TaskStateSpecific::Perpetual(specific) = &task.specific else {
                                        panic!();
                                    };
                                    specific.state.set((ProcState::Stopped, Utc::now()));
                                    specific.pid.set(None);
                                }
                                return Ok(true);
                            },
                            r = child.wait() => {
                                debug!(task = task_id, "Perpetual task exited, will restart after delay");
                                let mut logger = logger.await?;
                                if let Err(e) = logger.info(format!("Process ended with status: {:?}", r)) {
                                    warn!(err = e.to_string(), "Error sending message to syslog");
                                }

                                // Mark as starting + do state updates
                                {
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                                    let TaskStateSpecific::Perpetual(specific) = &task.specific else {
                                        panic!();
                                    };
                                    specific.state.set((ProcState::Starting, Utc::now()));
                                    on_stopping(&state_dynamic, &task_id);
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
                            warn!(err = e.to_string(), "Perpetual process failed with error");
                        },
                    }
                    select!{
                        _ = sleep(restart_delay) => {
                        },
                        _ =& mut stop_rx => {
                            break;
                        }
                    }
                }
            }.instrument(info_span!("task_perpetual", task_id = task.id)));
            return false;
        },
        TaskStateSpecific::Finite(s) => {
            if s.state.get().0 != ProcState::Stopped {
                return false;
            }

            // Mark as starting
            s.state.set((ProcState::Starting, Utc::now()));

            // Start
            let (stop_tx, mut stop_rx) = oneshot::channel();
            *s.stop.borrow_mut() = Some(stop_tx);
            let spec = s.spec.clone();
            let task_id = task.id.clone();
            let state = state.clone();
            spawn(async move {
                let restart_delay = Duration::from(spec.restart_delay.unwrap_or(SimpleDuration {
                    count: 1,
                    unit: puteron_lib::duration::SimpleDurationUnit::Minute,
                }).into());
                let mut success_codes = HashSet::new();
                success_codes.extend(spec.success_codes);
                if success_codes.is_empty() {
                    success_codes.insert(0);
                }
                loop {
                    match async {
                        ta_return!(bool, loga::Error);
                        let (mut child, pid, logger) = spawn_proc(&state.env, &task_id, &spec.command)?;
                        {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                            let TaskStateSpecific::Finite(specific) = &task.specific else {
                                panic!();
                            };
                            specific.pid.set(Some(pid.as_raw_nonzero().get()));
                        }

                        // Wait for exit
                        select!{
                            r = child.wait() => {
                                debug!(task = task_id, "Finite task exited, finished starting");
                                match r {
                                    Ok(r) => {
                                        if r.code().filter(|c| success_codes.contains(c)).is_some() {
                                            // Mark as started + do state updates
                                            {
                                                let mut state_dynamic = state.dynamic.lock().unwrap();
                                                let task =
                                                    &state_dynamic.task_alloc[*state_dynamic
                                                        .tasks
                                                        .get(&task_id)
                                                        .unwrap()];
                                                let TaskStateSpecific::Finite(specific) = &task.specific else {
                                                    panic!();
                                                };
                                                specific.failed_start_count.set(0);
                                                match specific.spec.started_action {
                                                    interface::task::FiniteTaskEndAction::None => {
                                                        specific.state.set((ProcState::Started, Utc::now()));
                                                        on_started(&state, &state_dynamic, &task_id);
                                                    },
                                                    interface::task::FiniteTaskEndAction::TurnOff => {
                                                        specific.state.set((ProcState::Stopped, Utc::now()));
                                                        set_task_user_off(&state_dynamic, &task_id);
                                                    },
                                                    interface::task::FiniteTaskEndAction::Delete => {
                                                        specific.state.set((ProcState::Stopped, Utc::now()));
                                                        delete_task(&mut state_dynamic, &task_id);
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
                                                let task =
                                                    &state_dynamic.task_alloc[*state_dynamic
                                                        .tasks
                                                        .get(&task_id)
                                                        .unwrap()];
                                                let TaskStateSpecific::Finite(specific) = &task.specific else {
                                                    panic!();
                                                };
                                                specific
                                                    .failed_start_count
                                                    .set(specific.failed_start_count.get() + 1);
                                            }

                                            // Keep as `starting`
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
                                debug!(task = task_id, "Got stop signal, stopping");

                                // Mark as stopping + do state updates
                                {
                                    let state_dynamic = state.dynamic.lock().unwrap();
                                    let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                                    let TaskStateSpecific::Finite(specific) = &task.specific else {
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
                                    let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                                    let TaskStateSpecific::Finite(specific) = &task.specific else {
                                        panic!();
                                    };
                                    specific.state.set((ProcState::Stopped, Utc::now()));
                                    specific.pid.set(None);
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
                            warn!(err = e.to_string(), "Perpetual process failed with error");
                        },
                    }
                    select!{
                        _ = sleep(restart_delay) => {
                        },
                        _ =& mut stop_rx => {
                            break;
                        }
                    }
                }
            }.instrument(info_span!("task_finite", task_id = task.id)));
            return false;
        },
        TaskStateSpecific::External => unreachable!(),
    }
}

fn set_task_user_on(state: &Arc<State>, state_dynamic: &StateDynamic, task_id: &TaskId) {
    let mut frontier = vec![(true, task_id.clone())];
    while let Some((first, task_id)) = frontier.pop() {
        if first {
            let Some(task) = state_dynamic.tasks.get(&task_id) else {
                continue;
            };
            let task = &state_dynamic.task_alloc[*task];
            let was_on = task_on(&task);
            task.transitive_on.set((true, Utc::now()));
            if was_on {
                continue;
            }
            frontier.push((false, task_id));
            upstream(&task, |dependencies| {
                for (dep_id, dep_type) in dependencies {
                    match dep_type {
                        DependencyType::Strong => { },
                        DependencyType::Weak => {
                            continue;
                        },
                    }
                    frontier.push((true, dep_id.clone()));
                }
            });
        } else {
            let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
            if all_upstream_tasks_started(state_dynamic, &task) {
                do_start_task(state, state_dynamic, &task);
            }
        }
    }

    // If already started all upstream + current, start downstream
    push_started(state, state_dynamic, task_id);
}

/// Return true if task is finished stopping (can continue with upstream).
fn do_stop_task(task: &TaskState_) -> bool {
    debug!(task = task.id, "Stopping task");
    match &task.specific {
        TaskStateSpecific::Empty(s) => {
            s.started.set((false, Utc::now()));
            return true;
        },
        TaskStateSpecific::Perpetual(s) => {
            if let Some(stop) = s.stop.take() {
                stop.send(()).unwrap();
                s.state.set((ProcState::Stopping, Utc::now()));
            }
            return false;
        },
        TaskStateSpecific::Finite(s) => {
            if let Some(stop) = s.stop.take() {
                stop.send(()).unwrap();
                s.state.set((ProcState::Stopping, Utc::now()));
            }
            return false;
        },
        TaskStateSpecific::External => unreachable!(),
    }
}

fn set_task_user_off(state_dynamic: &StateDynamic, task_id: &TaskId) {
    // Update on flags and check if the effective `on` state has changed
    {
        let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(task_id).unwrap()];
        let was_off = !task_on(&task);
        task.user_on.set((false, Utc::now()));
        if was_off || task.transitive_on.get().0 {
            return;
        }
    }

    // Stop downstream tasks starting from leaves to current task
    {
        let mut frontier = vec![(true, task_id.clone())];
        while let Some((first_pass, task_id)) = frontier.pop() {
            if first_pass {
                let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                if !task_started(&task) {
                    continue;
                }
                frontier.push((false, task_id));
                frontier.extend(task.downstream.borrow().keys().map(|k| (true, k.clone())));
            } else {
                let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
                if all_downstream_tasks_stopped(&state_dynamic, &task) {
                    do_stop_task(&task);
                }
            }
        }
    }

    // Unset transitive_on for strong upstream deps + stop, starting at root from
    // current task
    let mut frontier = vec![task_id.clone()];
    while let Some(task_id) = frontier.pop() {
        let Some(task) = state_dynamic.tasks.get(&task_id).cloned() else {
            continue;
        };
        let task = &state_dynamic.task_alloc[task];
        if !task_on(&task) {
            continue;
        }
        task.transitive_on.set((false, Utc::now()));
        if all_downstream_tasks_stopped(state_dynamic, &task) {
            do_stop_task(&task);
        }
        if task.user_on.get().0 {
            continue;
        }
        upstream(&task, |upstream| {
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
}

fn push_started(state: &Arc<State>, state_dynamic: &StateDynamic, from_task_id: &TaskId) {
    let mut frontier = vec![];
    {
        let start_task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(from_task_id).unwrap()];
        frontier.extend(start_task.downstream.borrow().keys().cloned());
    }
    while let Some(task_id) = frontier.pop() {
        let task = &state_dynamic.task_alloc[*state_dynamic.tasks.get(&task_id).unwrap()];
        if task_on(&task) {
            if do_start_task(state, state_dynamic, &task) {
                frontier.extend(task.downstream.borrow().keys().cloned());
            }
        }
    }
}
