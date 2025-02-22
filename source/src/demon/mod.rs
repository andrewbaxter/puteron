mod state;
mod schedule;
mod task_create_delete;
mod task_util;
mod task_execute;
mod task_plan;
mod task_plan_test;

use {
    crate::{
        interface::{
            self,
            base::TaskId,
            demon::Config,
            ipc::{
                ipc::{
                    self,
                    ServerResp,
                },
                ipc_path,
                Actual,
                RespScheduleEntry,
                TaskDownstreamStatus,
                TaskStatus,
                TaskUpstreamStatus,
            },
            task::{
                DependencyType,
                Task,
            },
        },
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
        ErrContext,
        Log,
        ResultContext,
    },
    schedule::{
        pop_schedule,
        populate_schedule,
    },
    state::{
        State,
        StateDynamic,
        TaskStateSpecific,
    },
    std::{
        collections::HashMap,
        env,
        sync::{
            Arc,
            Mutex,
        },
    },
    task_create_delete::{
        build_task,
        delete_task,
        validate_new_task,
    },
    task_execute::{
        set_task_user_off,
        set_task_user_on,
    },
    task_util::{
        get_task,
        is_task_effective_on,
        maybe_get_task,
        walk_task_upstream,
    },
    tokio::{
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
};

#[derive(Aargvark)]
pub struct DemonRunArgs {
    config: AargvarkJson<Config>,
    /// Validate that the config can be parsed and is valid per early checks (no
    /// dependency cycles, etc) and exit, don't run anything.
    validate: Option<()>,
}

pub async fn main(log: &Log, args: DemonRunArgs) -> Result<(), loga::Error> {
    let config = args.config.value;
    let mut specs = merge_specs(log, &config.task_dirs, None).await?;

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
        log: log.clone(),
        task_dirs: config.task_dirs,
        env: env,
        dynamic: Mutex::new(StateDynamic {
            task_alloc: Default::default(),
            tasks: Default::default(),
            schedule_top: Default::default(),
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
                let upstream: Vec<&String> = match &specs.get(task_id).unwrap() {
                    Task::Empty(s) => {
                        s.upstream.keys().collect()
                    },
                    Task::Long(s) => {
                        s.upstream.keys().collect()
                    },
                    Task::Short(s) => {
                        s.upstream.keys().collect()
                    },
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
        if args.validate.is_some() {
            return Ok(());
        }
    }

    // # Start async
    let mut schedule_next;
    {
        let mut state_dynamic = state.dynamic.lock().unwrap();

        // ## Start default-on tasks
        for (id, task) in state_dynamic.tasks.iter().map(|(x, y)| (x.clone(), y.clone())).collect::<Vec<_>>() {
            let task = &state_dynamic.task_alloc[task];
            let direct_on;
            match &task.specific {
                TaskStateSpecific::Empty(s) => {
                    direct_on = s.spec.default_on;
                },
                TaskStateSpecific::Long(s) => {
                    direct_on = s.spec.default_on;
                },
                TaskStateSpecific::Short(s) => {
                    direct_on = s.spec.default_on;
                },
            }
            if !direct_on {
                continue;
            }
            set_task_user_on(&state, &mut state_dynamic, &id);
        }

        // ## Schedule tasks
        populate_schedule(&mut state_dynamic);

        // Get initially scheduled task
        schedule_next = pop_schedule(&mut state_dynamic);
    }

    // ## Handle ipc + other inputs (signals)
    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt()).context("Error hooking into SIGINT")?;
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate()).context("Error hooking into SIGTERM")?;
    let state = state.clone();

    fn task_off_all(state: &Arc<State>) {
        let mut state_dynamic = state.dynamic.lock().unwrap();
        for task_id in state_dynamic.tasks.keys().cloned().collect::<Vec<_>>() {
            set_task_user_off(state, &mut state_dynamic, &task_id);
        }
    }

    let mut message_socket;
    if let Some(ipc_path) = ipc_path() {
        message_socket = Some(ipc::Server::new(ipc_path).await.map_err(loga::err)?);
    } else {
        message_socket = None;
    }
    let mut sigint = Box::pin(sigint.recv());
    let mut sigterm = Box::pin(sigterm.recv());
    loop {
        select!{
            _ =& mut sigint => {
                log.log(loga::DEBUG, "Got SIGINT, shutting down.");
                task_off_all(&state);
                break;
            },
            _ =& mut sigterm => {
                log.log(loga::DEBUG, "Got SIGTERM, shutting down.");
                task_off_all(&state);
                break;
            }
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
                let (_, spec) = schedule_next.unwrap();
                let mut state_dynamic = state.dynamic.lock().unwrap();
                log.log_with(
                    loga::DEBUG,
                    "Timer triggered for scheduled task, turning on.",
                    ea!(task = spec.0, schedule = spec.1.dbg_str()),
                );
                set_task_user_on(&state, &mut state_dynamic, &spec.0);
                state_dynamic
                    .schedule
                    .entry(schedule::calc_next_instant(Utc::now(), Instant::now(), &spec.1, false))
                    .or_default()
                    .push(spec);
                schedule_next = schedule::pop_schedule(&mut state_dynamic);
            }
        }
    }

    // Waits for all tasks
    state.tokio_tasks.close();
    state.tokio_tasks.wait().await;
    return Ok(());
}

async fn handle_ipc(state: Arc<State>, mut conn: ipc::ServerConn) {
    let log = state.log.fork(ea!(sys = "ipc"));
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
            async move {
                ta_return!(ipc::ServerResp, String);
                match req {
                    ipc::ServerReq::TaskList(rr, _) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        return Ok(rr(state_dynamic.tasks.keys().cloned().collect()));
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
                                    errors.into_iter().map(|x| format!("- {}", x)).collect::<Vec<_>>().join("\n")
                                ),
                            );
                        }

                        // # Create task
                        let user_on = match &m.spec {
                            Task::Empty(s) => s.default_on,
                            Task::Long(s) => s.default_on,
                            Task::Short(s) => s.default_on,
                        };
                        build_task(&mut state_dynamic, m.task.clone(), m.spec);

                        // # Turn on maybe
                        if user_on {
                            set_task_user_on(&state, &mut state_dynamic, &m.task);
                        }
                        return Ok(rr(()));
                    },
                    ipc::ServerReq::TaskDelete(rr, m) => {
                        let mut state_dynamic = state.dynamic.lock().unwrap();
                        let Some(task) = maybe_get_task(&state_dynamic, &m.0) else {
                            return Ok(rr(()));
                        };
                        if task.actual.get().0 != Actual::Stopped {
                            return Err(format!("Task isn't stopped yet"));
                        }
                        delete_task(&mut state_dynamic, &m.0);
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
                            effective_on: is_task_effective_on(task),
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
                            set_task_user_on(&state, &mut state_dynamic, &m.task);
                            return Ok(rr(()));
                        } else {
                            set_task_user_off(&state, &mut state_dynamic, &m.task);
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
                        let mut out = vec![];
                        for (task_id, state) in &state_dynamic.tasks {
                            if state_dynamic.task_alloc[*state].direct_on.get().0 {
                                out.push(task_id.clone());
                            }
                        }
                        return Ok(rr(out));
                    },
                    ipc::ServerReq::TaskListUpstream(rr, m) => {
                        let state_dynamic = state.dynamic.lock().unwrap();
                        if !state_dynamic.tasks.contains_key(&m.task) {
                            return Err(format!("Unknown task [{}]", m.task));
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
                            task_id: m.task.clone(),
                            dependency_type: DependencyType::Strong,
                        }];
                        while let Some(e) = frontier.pop() {
                            if e.first {
                                let task = get_task(&state_dynamic, &e.task_id);
                                let actual = task.actual.get().0;
                                if e.task_id != m.task && !m.include_started && actual == Actual::Started {
                                    continue;
                                }
                                frontier.push(Entry {
                                    first: false,
                                    task_id: e.task_id.clone(),
                                    dependency_type: e.dependency_type,
                                });
                                let push_status;
                                push_status = TaskUpstreamStatus {
                                    effective_on: is_task_effective_on(task),
                                    actual: actual,
                                    dependency_type: e.dependency_type,
                                    upstream: HashMap::new(),
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
                        if !state_dynamic.tasks.contains_key(&m.task) {
                            return Err(format!("Unknown task [{}]", m.task));
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
                            task_id: m.task.clone(),
                            dependency_type: DependencyType::Strong,
                            effective_dependency_type: DependencyType::Strong,
                        }];
                        while let Some(e) = frontier.pop() {
                            if e.first {
                                let task = get_task(&state_dynamic, &e.task_id);
                                let actual = task.actual.get().0;
                                if e.task_id != m.task && !m.include_stopped && actual == Actual::Stopped {
                                    continue;
                                }
                                frontier.push(Entry {
                                    first: false,
                                    task_id: e.task_id.clone(),
                                    dependency_type: e.dependency_type,
                                    effective_dependency_type: e.effective_dependency_type,
                                });
                                let push_status;
                                push_status = TaskDownstreamStatus {
                                    effective_on: is_task_effective_on(task),
                                    actual: task.actual.get().0,
                                    dependency_type: e.dependency_type,
                                    effective_dependency_type: e.effective_dependency_type,
                                    downstream: HashMap::new(),
                                };
                                for (down_id, down_type) in task.downstream.borrow().iter() {
                                    if !m.include_weak && *down_type == DependencyType::Weak {
                                        continue;
                                    }
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
                        return Ok(rr(state.env.clone()));
                    },
                    ipc::ServerReq::DemonSpecDirs(rr, _m) => {
                        return Ok(rr(state.task_dirs.clone()));
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
