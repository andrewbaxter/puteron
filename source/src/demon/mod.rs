mod state;
mod schedule;
mod task_create_delete;
mod task_util;
mod task_actual;
mod task_control;
mod task_control_test;

use {
    crate::{
        demon::task_create_delete::{
            delete_task_recursive_off,
            start_and_schedule_new_tasks,
        },
        errors::{
            ErrorHandler,
            LogErrorHandler,
            ReturnErrorHandler,
        },
        interface::{
            self,
            base::TaskId,
            demon::Config,
            ipc::{
                Actual,
                Event,
                EventType,
                RespScheduleEntry,
                TaskDownstreamStatus,
                TaskStatus,
                TaskUpstreamStatus,
                ipc::{
                    self,
                    ServerResp,
                },
                ipc_path,
            },
            task::{
                DependencyType,
                Task,
            },
        },
        spec::{
            list_task_dir_tasks,
            merge_specs,
        },
    },
    aargvark::{
        Aargvark,
        traits_impls::AargvarkJson,
    },
    chrono::Utc,
    flowcontrol::{
        shed,
        superif,
        ta_return,
    },
    loga::{
        DebugDisplay,
        ErrContext,
        Log,
        ResultContext,
        ea,
    },
    notify::Watcher,
    path_absolutize::Absolutize,
    schedule::{
        ScheduleEvent,
        pop_schedule,
    },
    sha2::{
        Digest,
        Sha256,
    },
    state::{
        State,
        StateDynamic,
        TaskStateSpecific,
    },
    std::{
        collections::{
            BTreeMap,
            BTreeSet,
            HashMap,
            HashSet,
        },
        env,
        path::PathBuf,
        sync::{
            Arc,
            Mutex,
        },
        time::Duration,
    },
    task_actual::{
        set_task_direct_off,
        set_task_direct_on,
    },
    task_create_delete::{
        build_task_noschedule,
        delete_task_immediate,
        validate_new_task,
    },
    task_util::{
        get_task,
        is_control_effective_on,
        maybe_get_task,
        walk_task_upstream,
    },
    tokio::{
        select,
        signal::unix::SignalKind,
        spawn,
        sync::{
            Notify,
            broadcast,
            oneshot,
        },
        time::{
            Instant,
            sleep,
            sleep_until,
        },
    },
    tokio_util::sync::CancellationToken,
};

#[derive(Aargvark)]
pub struct DemonRunArgs {
    config: AargvarkJson<Config>,
}

fn watcher_timeout() -> Duration {
    return Duration::from_secs(10);
}

async fn load_task_dirs_missing_noschedule(
    state: &State,
    errors: &mut dyn ErrorHandler,
) -> (HashSet<TaskId>, HashMap<TaskId, HashMap<PathBuf, Vec<u8>>>) {
    let mut new_tasks = HashSet::new();
    let mut fs_tasks = list_task_dir_tasks(&state.task_dirs, errors).await;
    let existing_task_specs;
    {
        let state_dynamic = state.dynamic.lock().unwrap();
        fs_tasks.retain(|k, _| !state_dynamic.tasks.contains_key(k));
        existing_task_specs = state_dynamic.tasks.keys().cloned().collect();
    }
    let new_specs = merge_specs(fs_tasks, errors).await;
    let (new_specs, new_hashes) = order_specs(existing_task_specs, errors, new_specs);
    let mut state_dynamic = state.dynamic.lock().unwrap();
    for (task_id, spec) in new_specs {
        new_tasks.insert(task_id.clone());
        validate_new_task(&state_dynamic, errors, &task_id, &spec);
        build_task_noschedule(&mut state_dynamic, task_id.clone(), spec, false);
    }
    return (new_tasks, new_hashes);
}

pub async fn main(debug: bool, log: &Log, args: DemonRunArgs) -> Result<(), loga::Error> {
    let config = args.config.value;

    // # Setup global env vars
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
        debug: debug,
        log_type: config.log_type,
        shutdown: Default::default(),
        log: log.clone(),
        task_dirs: config.task_dirs,
        env: env,
        dynamic: Mutex::new(StateDynamic {
            task_alloc: Default::default(),
            tasks: Default::default(),
            schedule_top: Default::default(),
            schedule: Default::default(),
            notify_reschedule: notify_reschedule.clone(),
            idle_watchers: Default::default(),
            watchers_send: Default::default(),
        }),
        tokio_tasks: Default::default(),
    });

    // # Setup tasks
    let tasks_sync_load = Arc::new(Notify::new());
    let loaded_hashes: Arc<Mutex<HashMap<TaskId, HashMap<PathBuf, Vec<u8>>>>> = Default::default();
    let mut _watcher = None;
    if config.watch {
        let tasks_sync_delete: Arc<dyn Send + Sync + Fn() -> ()> = Arc::new({
            let bg = Arc::new(Mutex::new(None));
            let state = state.clone();
            let tasks_sync_load = tasks_sync_load.clone();
            let loaded_hashes = loaded_hashes.clone();
            move || {
                let ct = CancellationToken::new();
                *bg.lock().unwrap() = Some(ct.clone().drop_guard());
                let state = state.clone();
                let tasks_sync_load = tasks_sync_load.clone();
                let loaded_hashes = loaded_hashes.clone();
                spawn(async move {
                    let work = async {
                        sleep(Duration::from_secs(1)).await;
                        let mut errors = LogErrorHandler { log: state.log.clone() };
                        let fs_tasks = list_task_dir_tasks(&state.task_dirs, &mut errors).await;
                        let mut remove_tasks = HashSet::new();
                        {
                            let mut state_dynamic = state.dynamic.lock().unwrap();
                            for (task_id, task) in &state_dynamic.tasks {
                                let task = &state_dynamic.task_alloc[*task];
                                if task.cli_created {
                                    continue;
                                }
                                superif!({
                                    let Some(paths) = fs_tasks.get(task_id) else {
                                        break 'remove;
                                    };
                                    for path in paths {
                                        let bytes =
                                            match std::fs::read(
                                                &path,
                                            ).context_with(
                                                "Error reading json from task directory",
                                                ea!(path = path.to_string_lossy())
                                            ) {
                                                Ok(v) => v,
                                                Err(_) => {
                                                    break 'remove;
                                                },
                                            };
                                        let fs_hash = Sha256::digest(&bytes).to_vec();
                                        match loaded_hashes
                                            .lock()
                                            .unwrap()
                                            .get(task_id)
                                            .and_then(|m| m.get(path))
                                            .cloned() {
                                            Some(loaded_hash) => {
                                                if loaded_hash != fs_hash {
                                                    break 'remove;
                                                }
                                            },
                                            None => {
                                                break 'remove;
                                            },
                                        }
                                    }
                                } 'remove {
                                    loaded_hashes.lock().unwrap().remove(task_id);
                                    remove_tasks.insert(task_id.clone());
                                });
                            }
                            for task in &remove_tasks {
                                state
                                    .log
                                    .log(
                                        loga::DEBUG,
                                        &format!(
                                            "Task [{}] disk data changed, deleting in preparation for reload.",
                                            task
                                        ),
                                    );
                                delete_task_recursive_off(&state, &mut *state_dynamic, task, true);
                            }
                        }
                        for task in remove_tasks {
                            let (notify_tx, notify_rx) = oneshot::channel();
                            {
                                let state_dynamic = state.dynamic.lock().unwrap();
                                let Some(task) = maybe_get_task(&state_dynamic, &task) else {
                                    continue;
                                };
                                task.stopped_waiters.borrow_mut().push(notify_tx);
                            }
                            notify_rx.await.ignore();
                        }
                        tasks_sync_load.notify_one();
                    };
                    select!{
                        _ = ct.cancelled() => {
                        },
                        _ = work => {
                        }
                    }
                });
            }
        });
        let mut root_watcher = notify::recommended_watcher({
            let tasks_sync_delete = tasks_sync_delete.clone();
            move |_ev: Result<notify::Event, notify::Error>| {
                tasks_sync_delete();
            }
        }).context("Error creating task dir watcher")?;
        let mut abs_task_dirs = HashSet::new();
        for dir in &state.task_dirs {
            abs_task_dirs.insert(dir.absolutize().context(&format!("Invalid path [{:?}]", dir))?.to_path_buf());
        }
        let mut root_parent_watcher = notify::recommended_watcher({
            let state = state.clone();
            let mut sub_watchers = HashMap::new();
            let tasks_sync_delete = tasks_sync_delete.clone();
            let task_dirs = abs_task_dirs.clone();
            move |ev: Result<notify::Event, notify::Error>| {
                match ev {
                    Ok(ev) => {
                        for path in ev.paths {
                            let path = path.absolutize().unwrap().to_path_buf();
                            if !task_dirs.contains(&path) {
                                continue;
                            }
                            let mut watcher = match notify::recommended_watcher({
                                let tasks_sync_delete = tasks_sync_delete.clone();
                                move |_ev: Result<notify::Event, notify::Error>| {
                                    tasks_sync_delete();
                                }
                            }) {
                                Ok(v) => v,
                                Err(e) => {
                                    state.log.log_err(loga::WARN, e.context("Error creating task dir watcher"));
                                    continue;
                                },
                            };
                            if path.is_symlink() {
                                let path = match path.read_link() {
                                    Ok(p) => p,
                                    Err(e) => {
                                        state
                                            .log
                                            .log_err(
                                                loga::DEBUG,
                                                e.context(
                                                    "Failed to dereference task directory path at time of detected change",
                                                ),
                                            );
                                        continue;
                                    },
                                };
                                watcher
                                    .watch(&path, notify::RecursiveMode::NonRecursive)
                                    .log(
                                        &state.log,
                                        loga::WARN,
                                        &format!("Error watching task dir symlink [{:?}]", path),
                                    );
                            } else {
                                watcher
                                    .watch(&path, notify::RecursiveMode::NonRecursive)
                                    .log(&state.log, loga::WARN, &format!("Error watching task dir [{:?}]", path));
                            }
                            sub_watchers.insert(path, watcher);
                            tasks_sync_delete();
                        }
                    },
                    Err(e) => {
                        state.log.log_err(loga::WARN, e.context("Error receiving directory watch event"));
                    },
                }
            }
        }).context("Error creating task dir watcher")?;
        for dir in abs_task_dirs {
            if let Some(dir_parent) = dir.parent() {
                root_parent_watcher
                    .watch(dir_parent, notify::RecursiveMode::NonRecursive)
                    .context(&format!("Error watching task dir parent dir [{:?}]", dir_parent))?;
            } else {
                root_watcher
                    .watch(&dir, notify::RecursiveMode::NonRecursive)
                    .context(&format!("Error watching task dir [{:?}]", dir))?;
            }
        }
        _watcher = Some((root_parent_watcher, root_watcher));
        tasks_sync_load.notify_one();
    } else {
        let mut errors = LogErrorHandler { log: state.log.clone() };
        let (new_tasks, _) = load_task_dirs_missing_noschedule(&state, &mut errors).await;
        let mut state_dynamic = state.dynamic.lock().unwrap();
        start_and_schedule_new_tasks(&state, &mut *state_dynamic, new_tasks);
    }

    // ## Handle ipc + other inputs (signals)
    let mut schedule_next = None;
    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt()).context("Error hooking into SIGINT")?;
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate()).context("Error hooking into SIGTERM")?;
    let state = state.clone();
    {
        fn task_off_all(state: &Arc<State>) {
            let mut state_dynamic = state.dynamic.lock().unwrap();
            for task_id in state_dynamic.tasks.keys().cloned().collect::<Vec<_>>() {
                set_task_direct_off(state, &mut state_dynamic, &task_id);
            }
        }

        let mut message_socket;
        if let Some(ipc_path) = ipc_path() {
            message_socket = Some(ipc::Server::new(ipc_path).await.map_err(loga::err)?);
        } else {
            message_socket = None;
        }
        loop {
            select!{
                // # Server control
                _ = sigint.recv() => {
                    log.log(loga::DEBUG, "Got SIGINT, shutting down.");
                    task_off_all(&state);
                    break;
                },
                _ = sigterm.recv() => {
                    log.log(loga::DEBUG, "Got SIGTERM, shutting down.");
                    task_off_all(&state);
                    break;
                }
                // # Task dir watch events
                _ = tasks_sync_load.notified() => {
                    state.log.log(loga::DEBUG, "Stale tasks deleted, re-launching them from disk");
                    let mut errors = LogErrorHandler { log: state.log.clone() };
                    let (new_tasks, hashes) = load_task_dirs_missing_noschedule(&state, &mut errors).await;
                    loaded_hashes.lock().unwrap().extend(hashes);
                    {
                        let mut state_dynamic = state.dynamic.lock().unwrap();
                        start_and_schedule_new_tasks(&state, &mut *state_dynamic, new_tasks);
                    }
                },
                // # IPC events
                accepted = message_socket.as_mut().unwrap().accept(),
                if message_socket.is_some() => {
                    let stream = match accepted {
                        Ok(x) => x,
                        Err(e) => {
                            log.log_err(loga::DEBUG, loga::err(e).context("Error accepting connection"));
                            continue;
                        },
                    };
                    spawn(handle_ipc(state.clone(), stream));
                },
                // # Schedule events
                _ = notify_reschedule.notified() => {
                    let mut state_dynamic = state.dynamic.lock().unwrap();
                    if let Some((delay, spec)) = schedule_next {
                        state_dynamic.schedule.entry(delay).or_default().push(spec);
                    }
                    schedule_next = pop_schedule(&mut state_dynamic);
                },
                _ = async {
                    if let Some((delay, _)) = schedule_next.as_ref() {
                        sleep_until(*delay).await;
                    }
                },
                if schedule_next.is_some() => {
                    let (_, event) = schedule_next.unwrap();
                    let mut state_dynamic = state.dynamic.lock().unwrap();
                    match event {
                        ScheduleEvent::Rule(spec) => {
                            log.log_with(
                                loga::DEBUG,
                                "Timer triggered for scheduled task, turning on.",
                                ea!(task = spec.0, schedule = spec.1.dbg_str()),
                            );
                            set_task_direct_on(&state, &mut state_dynamic, &spec.0);
                            state_dynamic
                                .schedule
                                .entry(schedule::calc_next_instant(Utc::now(), Instant::now(), &spec.1, false))
                                .or_default()
                                .push(ScheduleEvent::Rule(spec));
                        },
                        ScheduleEvent::WatcherExpire(pid) => {
                            if let Some((last_active, _receiver)) = state_dynamic.idle_watchers.get(&pid) {
                                if Instant::now().duration_since(*last_active) > watcher_timeout() {
                                    state_dynamic.idle_watchers.remove(&pid);
                                }
                            }
                        },
                    }
                    schedule_next = schedule::pop_schedule(&mut state_dynamic);
                }
            }
        }
    }

    // Waits for all tasks
    state.tokio_tasks.close();
    state.shutdown.cancel();
    state.tokio_tasks.wait().await;
    return Ok(());
}

async fn handle_ipc(state: Arc<State>, mut conn: ipc::ServerConn) {
    let log = state.log.fork(ea!(sys = "ipc"));
    let peer = conn.0.peer_cred().map_err(|e| format!("Error getting IPC connection peer information: {}", e));
    loop {
        let req = match conn.recv_req().await {
            Ok(Some(message)) => message,
            Ok(None) => {
                return;
            },
            Err(e) => {
                log.log_err(loga::DEBUG, loga::err(e).context("Error reading message from connection"));
                return;
            },
        };
        let resp = {
            let state = state.clone();
            let log = log.clone();
            let peer = peer.clone();
            async move {
                ta_return!(ipc::ServerResp, String);
                match req {
                    ipc::ServerReq::TaskList(rr, _) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        return Ok(rr(state_dynamic.tasks.keys().cloned().collect()));
                    },
                    ipc::ServerReq::TaskWatch(rr, _) => {
                        let peer = peer?;
                        let pid = peer.pid().ok_or_else(|| format!("IPC connection missing PID information"))?;
                        let mut out = vec![];

                        // Create or get existing receiver if pid already subscribed
                        let mut receiver = shed!{
                            let mut state_dynamic = state.dynamic.lock().unwrap();
                            if let Some((_, receiver)) = state_dynamic.idle_watchers.remove(&pid) {
                                break receiver;
                            };
                            for (task_id, task) in state_dynamic.tasks.iter() {
                                let task = &state_dynamic.task_alloc[*task];
                                out.push(Event {
                                    task: task_id.clone(),
                                    event: EventType::DirectOn(task.direct_on.get().0),
                                });
                                out.push(Event {
                                    task: task_id.clone(),
                                    event: EventType::TransitiveOn(task.transitive_on.get().0),
                                });
                                out.push(Event {
                                    task: task_id.clone(),
                                    event: EventType::EffectiveOn(is_control_effective_on(task)),
                                });
                                out.push(Event {
                                    task: task_id.clone(),
                                    event: EventType::Actual(task.actual.get().0),
                                });
                            }
                            let sender =
                                state_dynamic.watchers_send.take().unwrap_or_else(|| broadcast::Sender::new(1000));
                            let receiver = sender.subscribe();
                            *state_dynamic.watchers_send.borrow_mut() = Some(sender);
                            break receiver;
                        };

                        // Read queued events or wait for next event
                        'done_reading : loop {
                            // Read anything queued
                            loop {
                                match receiver.try_recv() {
                                    Ok(v) => {
                                        out.push(v);
                                    },
                                    Err(e) => match e {
                                        broadcast::error::TryRecvError::Empty => {
                                            if out.is_empty() {
                                                break;
                                            } else {
                                                break 'done_reading;
                                            }
                                        },
                                        broadcast::error::TryRecvError::Closed => {
                                            break 'done_reading;
                                        },
                                        broadcast::error::TryRecvError::Lagged(_) => {
                                            return Err(format!("Too slow reading events, connection broken"));
                                        },
                                    },
                                }
                            }

                            // Wait for next if none yet, then read anything queued
                            match receiver.recv().await {
                                Ok(v) => {
                                    out.push(v);
                                },
                                Err(e) => match e {
                                    broadcast::error::RecvError::Closed => {
                                        break 'done_reading;
                                    },
                                    broadcast::error::RecvError::Lagged(_) => {
                                        return Err(format!("Too slow reading events, connection broken"));
                                    },
                                },
                            }
                        }

                        // Park receiver until next ipc or it expires
                        {
                            let mut state_dynamic = state.dynamic.lock().unwrap();
                            state_dynamic.idle_watchers.insert(pid, (Instant::now(), receiver));
                            state_dynamic
                                .schedule
                                .entry(Instant::now() + watcher_timeout())
                                .or_default()
                                .push(ScheduleEvent::WatcherExpire(pid));
                            state_dynamic.notify_reschedule.notify_one();
                        }

                        // Respond
                        return Ok(rr(out));
                    },
                    ipc::ServerReq::TaskAdd(rr, m) => {
                        let mut state_dynamic = state.dynamic.lock().unwrap();

                        // # Check + delete the old task if it exists
                        if let Some(task) = maybe_get_task(&state_dynamic, &m.task) {
                            if !m.unique {
                                return Err(format!("A task with this ID already exists"));
                            }
                            if task.actual.get().0 != Actual::Stopped {
                                return Err(format!("Task isn't stopped yet"));
                            }
                            let same = match (&m.spec, &task.specific) {
                                (Task::Empty(new), TaskStateSpecific::Empty(old)) => new == &old.spec,
                                (Task::Long(new), TaskStateSpecific::Long(old)) => new == &old.spec,
                                (Task::Short(new), TaskStateSpecific::Short(old)) => new == &old.spec,
                                _ => false,
                            };
                            if same {
                                return Ok(rr(()));
                            }
                            delete_task_immediate(&mut state_dynamic, &m.task);
                        }

                        // # Check new task spec
                        //
                        // Check for broken upstreams
                        let mut errors = ReturnErrorHandler { errors: Default::default() };
                        validate_new_task(&state_dynamic, &mut errors, &m.task, &m.spec);
                        if !errors.errors.is_empty() {
                            return Err(
                                format!(
                                    "Task has errors:\n{}",
                                    errors
                                        .errors
                                        .into_iter()
                                        .map(|x| format!("- {}", x))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                ),
                            );
                        }

                        // # Create task
                        let direct_on = match &m.spec {
                            Task::Empty(s) => s.default_on,
                            Task::Long(s) => s.default_on,
                            Task::Short(s) => s.default_on,
                        };
                        build_task_noschedule(&mut state_dynamic, m.task.clone(), m.spec, true);
                        start_and_schedule_new_tasks(
                            &state,
                            &mut state_dynamic,
                            [m.task.clone()].into_iter().collect(),
                        );

                        // # Turn on maybe
                        if direct_on {
                            set_task_direct_on(&state, &mut state_dynamic, &m.task);
                        }
                        return Ok(rr(()));
                    },
                    ipc::ServerReq::TaskDelete(rr, m) => {
                        {
                            let mut state_dynamic = state.dynamic.lock().unwrap();
                            let Some(task) = maybe_get_task(&state_dynamic, &m.task) else {
                                return Ok(rr(()));
                            };
                            if !m.recurse {
                                if task.actual.get().0 != Actual::Stopped {
                                    return Err(format!("Cannot delete task [{}], it isn't stopped yet", m.task));
                                }
                                for (down_id, _down_type) in task.downstream.borrow().iter() {
                                    let down_task = get_task(&state_dynamic, down_id);
                                    if !down_task.delete_when_stopped.get() {
                                        return Err(
                                            format!("Downstream task [{}] isn't marked for deletion yet.", down_id),
                                        );
                                    }
                                }
                                delete_task_immediate(&mut state_dynamic, &m.task);
                            } else {
                                delete_task_recursive_off(&state, &mut state_dynamic, &m.task, m.off);
                            }
                        }
                        if m.wait {
                            let (notify_tx, notify_rx) = oneshot::channel();
                            {
                                let state_dynamic = state.dynamic.lock().unwrap();
                                let Some(task) = maybe_get_task(&state_dynamic, &m.task) else {
                                    return Ok(rr(()));
                                };
                                task.stopped_waiters.borrow_mut().push(notify_tx);
                            }
                            notify_rx.await.ignore();
                        }
                        return Ok(rr(()));
                    },
                    ipc::ServerReq::TaskGetStatus(rr, m) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                            return Err(format!("Unknown task [{}]", m.0));
                        };
                        return Ok(rr(TaskStatus {
                            direct_on: task.direct_on.get().0,
                            direct_on_at: task.direct_on.get().1,
                            transitive_on: task.transitive_on.get().0,
                            transitive_on_at: task.transitive_on.get().1,
                            effective_on: is_control_effective_on(task),
                            actual: task.actual.get().0,
                            actual_at: task.actual.get().1,
                            specific: match &task.specific {
                                TaskStateSpecific::Empty(_) => interface::ipc::TaskStatusSpecific::Empty(
                                    interface::ipc::TaskStatusSpecificEmpty {},
                                ),
                                TaskStateSpecific::Long(s) => interface::ipc::TaskStatusSpecific::Long(
                                    interface::ipc::TaskStatusSpecificLong {
                                        pid: s.pid.get(),
                                        restarts: s.failed_start_count.get(),
                                    },
                                ),
                                TaskStateSpecific::Short(s) => interface::ipc::TaskStatusSpecific::Short(
                                    interface::ipc::TaskStatusSpecificShort {
                                        pid: s.pid.get(),
                                        restarts: s.failed_start_count.get(),
                                    },
                                ),
                            },
                        }));
                    },
                    ipc::ServerReq::TaskGetSpec(rr, m) => {
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
                        }
                        return Ok(rr(out));
                    },
                    ipc::ServerReq::TaskOnOff(rr, m) => {
                        let mut state_dynamic = state.dynamic.lock().unwrap();
                        if !state_dynamic.tasks.contains_key(&m.task) {
                            return Err(format!("Unknown task [{}]", m.task));
                        }
                        if m.on {
                            set_task_direct_on(&state, &mut state_dynamic, &m.task);
                            return Ok(rr(()));
                        } else {
                            set_task_direct_off(&state, &mut state_dynamic, &m.task);
                            return Ok(rr(()));
                        }
                    },
                    ipc::ServerReq::TaskWaitRunning(rr, m) => {
                        let (notify_tx, notify_rx) = oneshot::channel();
                        {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                                return Err(format!("Unknown task [{}]", m.0));
                            };
                            if task.actual.get().0 == Actual::Started {
                                return Ok(rr(()));
                            }
                            task.started_waiters.borrow_mut().push(notify_tx);
                        }
                        if notify_rx.await.map_err(|e| e.to_string())? {
                            return Ok(rr(()));
                        } else {
                            return Err("Start canceled; task is now stopping".to_string());
                        }
                    },
                    ipc::ServerReq::TaskWaitStopped(rr, m) => {
                        let (notify_tx, notify_rx) = oneshot::channel();
                        {
                            let state_dynamic = state.dynamic.lock().unwrap();
                            let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                                return Err(format!("Unknown task [{}]", m.0));
                            };
                            if task.actual.get().0 == Actual::Stopped {
                                return Ok(rr(()));
                            }
                            task.stopped_waiters.borrow_mut().push(notify_tx);
                        }
                        match notify_rx.await {
                            Ok(res) => {
                                if res {
                                    return Ok(rr(()));
                                } else {
                                    return Err("Stop canceled; task is now starting".to_string());
                                }
                            },
                            Err(e) => {
                                return Err(e.to_string());
                            },
                        }
                    },
                    ipc::ServerReq::TaskListUserOn(rr, _m) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        let mut out = BTreeSet::new();
                        for (task_id, state) in &state_dynamic.tasks {
                            if state_dynamic.task_alloc[*state].direct_on.get().0 {
                                out.insert(task_id.clone());
                            }
                        }
                        return Ok(rr(out));
                    },
                    ipc::ServerReq::TaskListBlockingStart(rr, m) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        if !state_dynamic.tasks.contains_key(&m.0) {
                            return Err(format!("Unknown task [{}]", m.0));
                        }
                        let mut leaves = BTreeSet::new();

                        struct Entry {
                            task_id: TaskId,
                        }

                        let mut frontier = vec![Entry { task_id: m.0.clone(), }];
                        while let Some(e) = frontier.pop() {
                            let task = get_task(&state_dynamic, &e.task_id);
                            if task.actual.get().0 != Actual::Started {
                                leaves.insert(e.task_id.clone());
                            } else {
                                walk_task_upstream(task, |upstream| {
                                    for (up_id, _up_type) in upstream {
                                        frontier.push(Entry { task_id: up_id.clone() });
                                    }
                                });
                            }
                        }
                        return Ok(rr(leaves));
                    },
                    ipc::ServerReq::TaskListBlockingStop(rr, m) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        if !state_dynamic.tasks.contains_key(&m.0) {
                            return Err(format!("Unknown task [{}]", m.0));
                        }
                        let mut leaves = BTreeSet::new();

                        struct Entry {
                            task_id: TaskId,
                        }

                        let mut frontier = vec![Entry { task_id: m.0.clone(), }];
                        while let Some(e) = frontier.pop() {
                            let task = get_task(&state_dynamic, &e.task_id);
                            if task.actual.get().0 != Actual::Stopped {
                                leaves.insert(e.task_id.clone());
                            } else {
                                for (down_id, _down_type) in task.downstream.borrow().iter() {
                                    frontier.push(Entry { task_id: down_id.clone() });
                                }
                            }
                        }
                        return Ok(rr(leaves));
                    },
                    ipc::ServerReq::TaskListUpstream(rr, m) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        if !state_dynamic.tasks.contains_key(&m.0) {
                            return Err(format!("Unknown task [{}]", m.0));
                        }
                        let mut out_stack = vec![];
                        let mut root = None;

                        struct Entry {
                            first: bool,
                            task_id: TaskId,
                            dependency_type: DependencyType,
                        }

                        let mut frontier = vec![Entry {
                            first: true,
                            task_id: m.0.clone(),
                            dependency_type: DependencyType::Strong,
                        }];
                        while let Some(e) = frontier.pop() {
                            if e.first {
                                let task = get_task(&state_dynamic, &e.task_id);
                                let actual = task.actual.get().0;
                                frontier.push(Entry {
                                    first: false,
                                    task_id: e.task_id.clone(),
                                    dependency_type: e.dependency_type,
                                });
                                let push_status;
                                push_status = TaskUpstreamStatus {
                                    effective_on: is_control_effective_on(task),
                                    actual: actual,
                                    dependency_type: e.dependency_type,
                                    upstream: Default::default(),
                                };
                                walk_task_upstream(task, |upstream| {
                                    for (up_id, up_type) in upstream {
                                        frontier.push(Entry {
                                            first: true,
                                            task_id: up_id.clone(),
                                            dependency_type: *up_type,
                                        });
                                    }
                                });
                                out_stack.push((e.task_id, push_status));
                            } else {
                                let (top_id, top) = out_stack.pop().unwrap();
                                if let Some(parent) = out_stack.last_mut() {
                                    parent.1.upstream.insert(top_id, top);
                                } else {
                                    root = Some(top.upstream);
                                }
                            }
                        }
                        return Ok(rr(root.unwrap()));
                    },
                    ipc::ServerReq::TaskListDownstream(rr, m) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        if !state_dynamic.tasks.contains_key(&m.0) {
                            return Err(format!("Unknown task [{}]", m.0));
                        }
                        let mut out_stack = vec![];
                        let mut root = None;

                        struct Entry {
                            first: bool,
                            task_id: TaskId,
                            dependency_type: DependencyType,
                            effective_dependency_type: DependencyType,
                        }

                        let mut frontier = vec![Entry {
                            first: true,
                            task_id: m.0.clone(),
                            dependency_type: DependencyType::Strong,
                            effective_dependency_type: DependencyType::Strong,
                        }];
                        while let Some(e) = frontier.pop() {
                            if e.first {
                                let task = get_task(&state_dynamic, &e.task_id);
                                frontier.push(Entry {
                                    first: false,
                                    task_id: e.task_id.clone(),
                                    dependency_type: e.dependency_type,
                                    effective_dependency_type: e.effective_dependency_type,
                                });
                                let push_status;
                                push_status = TaskDownstreamStatus {
                                    effective_on: is_control_effective_on(task),
                                    actual: task.actual.get().0,
                                    dependency_type: e.dependency_type,
                                    effective_dependency_type: e.effective_dependency_type,
                                    downstream: Default::default(),
                                };
                                for (down_id, down_type) in task.downstream.borrow().iter() {
                                    frontier.push(Entry {
                                        first: true,
                                        task_id: down_id.clone(),
                                        dependency_type: *down_type,
                                        effective_dependency_type: match e.effective_dependency_type {
                                            DependencyType::Strong => *down_type,
                                            DependencyType::Weak => DependencyType::Weak,
                                        },
                                    });
                                }
                                out_stack.push((e.task_id, push_status));
                            } else {
                                let (top_id, top) = out_stack.pop().unwrap();
                                if let Some(parent) = out_stack.last_mut() {
                                    parent.1.downstream.insert(top_id, top);
                                } else {
                                    root = Some(top.downstream);
                                }
                            }
                        }
                        return Ok(rr(root.unwrap()));
                    },
                    ipc::ServerReq::DemonListSchedule(rr, _m) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        let instant_now = Instant::now();
                        let now = Utc::now();
                        let mut out = vec![];
                        out.reserve(state_dynamic.schedule.len() + 1);
                        #[allow(for_loops_over_fallibles)]
                        for (at, entry) in &state_dynamic.schedule_top {
                            let ScheduleEvent::Rule(entry) = entry else {
                                continue;
                            };
                            let at_secs: i64 = match at.duration_since(instant_now).as_secs().try_into() {
                                Ok(s) => s,
                                Err(e) => {
                                    log.log_err(
                                        loga::WARN,
                                        e.context_with(
                                            "Schedule entry out of i64 range for chrono IPC response",
                                            ea!(task = entry.0, rule = entry.1.dbg_str()),
                                        ),
                                    );
                                    continue;
                                },
                            };
                            out.push(RespScheduleEntry {
                                at: now + chrono::Duration::seconds(at_secs),
                                task: entry.0.clone(),
                                rule: entry.1.clone(),
                            });
                        }
                        for (at, entries) in &state_dynamic.schedule {
                            for entry in entries {
                                let ScheduleEvent::Rule(entry) = entry else {
                                    continue;
                                };
                                let at_secs: i64 = match at.duration_since(instant_now).as_secs().try_into() {
                                    Ok(s) => s,
                                    Err(e) => {
                                        log.log_err(
                                            loga::WARN,
                                            e.context_with(
                                                "Schedule entry out of i64 range for chrono IPC response",
                                                ea!(task = entry.0, rule = entry.1.dbg_str()),
                                            ),
                                        );
                                        continue;
                                    },
                                };
                                out.push(RespScheduleEntry {
                                    at: now + chrono::Duration::seconds(at_secs),
                                    task: entry.0.clone(),
                                    rule: entry.1.clone(),
                                });
                            }
                        }
                        return Ok(rr(out));
                    },
                    ipc::ServerReq::DemonEnv(rr, _m) => {
                        return Ok(rr(state.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect()));
                    },
                    ipc::ServerReq::DemonSpecDirs(rr, _m) => {
                        return Ok(rr(state.task_dirs.iter().cloned().collect()));
                    },
                }
            }
        }.await.unwrap_or_else(ServerResp::err);
        match conn.send_resp(resp).await {
            Ok(_) => { },
            Err(e) => {
                log.log_err(loga::DEBUG, loga::err(e).context("Error writing response"));
            },
        }
    }
}
