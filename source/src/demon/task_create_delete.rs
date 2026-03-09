use {
    super::{
        schedule::{
            ScheduleEvent,
            ScheduleRule,
            calc_next_instant,
        },
        state::{
            StateDynamic,
            TaskState_,
            TaskStateEmpty,
            TaskStateLong,
            TaskStateShort,
            TaskStateSpecific,
        },
        task_util::{
            get_task,
            is_control_effective_on,
            maybe_get_task,
            walk_task_upstream,
        },
    },
    crate::{
        demon::{
            state::State,
            task_actual::{
                set_task_direct_off,
                set_task_direct_on,
            },
        },
        interface::{
            self,
            base::TaskId,
            ipc::{
                Actual,
                Event,
                EventType,
            },
            task::Task,
        },
    },
    chrono::Utc,
    std::{
        cell::{
            Cell,
            RefCell,
        },
        collections::HashSet,
        sync::Arc,
    },
    tokio::time::Instant,
};

pub(crate) fn validate_new_task(
    state_dynamic: &StateDynamic,
    errors: &mut Vec<loga::Error>,
    task_id: &TaskId,
    task: &interface::task::Task,
) {
    let upstream: Vec<&String> = match task {
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
    for upstream_id in upstream {
        let Some(upstream_task) = maybe_get_task(&state_dynamic, &upstream_id) else {
            errors.push(loga::err(format!("Task [{}] has missing upstream [{}]", task_id, upstream_id)));
            continue;
        };
        match &upstream_task.specific {
            TaskStateSpecific::Empty(_s) => { },
            TaskStateSpecific::Long(_s) => { },
            TaskStateSpecific::Short(s) => {
                if let Some(started_action) = &s.spec.started_action {
                    match started_action {
                        interface::task::ShortTaskStartedAction::None => { },
                        interface::task::ShortTaskStartedAction::TurnOff => {
                            errors.push(
                                loga::err(
                                    format!(
                                        "Task [{}] upstream [{}] has started action turn_off so this task will never be able to start",
                                        task_id,
                                        upstream_id
                                    ),
                                ),
                            );
                        },
                    }
                }
            },
        }
    }
}

pub(crate) fn start_and_schedule_new_tasks(state: &Arc<State>, 
state_dynamic: &mut StateDynamic, 
    new_tasks: HashSet<TaskId>) {
    // ## Start default-on tasks
    for id in &new_tasks {
        let task = state_dynamic.tasks[id];
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
        set_task_direct_on(state, state_dynamic, &id);
    }

    // ## Schedule tasks
    //
    // Don't know why, split mut/immut borrowing failed here...
    let mut schedule = vec![];
    for (id, task) in &state_dynamic.tasks {
        let task = &state_dynamic.task_alloc[*task];
        if let TaskStateSpecific::Short(t) = &task.specific {
            for rule in &t.spec.schedule {
                schedule.push((
                    // k
                    calc_next_instant(Utc::now(), Instant::now(), rule, false),
                    // v
                    ScheduleEvent::Rule(ScheduleRule::new((id.clone(), rule.clone()))),
                ));
            }
        }
    }
    let has_schedule = !schedule.is_empty();
    for (k, v) in schedule {
        state_dynamic.schedule.entry(k).or_default().push(v);
    }
    if has_schedule {
        state_dynamic.notify_reschedule.notify_one();
    }
}

pub(crate) fn build_task_noschedule(state_dynamic: &mut StateDynamic, task_id: TaskId, spec: Task) {
    let specific;
    let delete_when_stopped;
    match spec {
        interface::task::Task::Empty(spec) => {
            for (upstream_id, upstream_type) in &spec.upstream {
                get_task(state_dynamic, upstream_id)
                    .downstream
                    .borrow_mut()
                    .insert(task_id.clone(), upstream_type.clone());
            }
            delete_when_stopped = spec.delete_when_stopped;
            specific = TaskStateSpecific::Empty(TaskStateEmpty { spec: spec });
        },
        interface::task::Task::Long(spec) => {
            for (upstream_id, upstream_type) in &spec.upstream {
                get_task(state_dynamic, &upstream_id)
                    .downstream
                    .borrow_mut()
                    .insert(task_id.clone(), upstream_type.clone());
            }
            delete_when_stopped = spec.delete_when_stopped;
            specific = TaskStateSpecific::Long(TaskStateLong {
                spec: spec,
                stop: RefCell::new(None),
                pid: Cell::new(None),
                failed_start_count: Cell::new(0),
            });
        },
        interface::task::Task::Short(spec) => {
            for (upstream_id, upstream_type) in &spec.upstream {
                get_task(state_dynamic, &upstream_id)
                    .downstream
                    .borrow_mut()
                    .insert(task_id.clone(), upstream_type.clone());
            }
            delete_when_stopped = spec.delete_when_stopped;
            specific = TaskStateSpecific::Short(TaskStateShort {
                spec: spec,
                stop: RefCell::new(None),
                pid: Cell::new(None),
                failed_start_count: Cell::new(0),
            });
        },
    }
    let task = state_dynamic.task_alloc.insert(TaskState_ {
        id: task_id.clone(),
        direct_on: Cell::new((false, Utc::now())),
        transitive_on: Cell::new((false, Utc::now())),
        awueo: Cell::new({
            let mut all_on = true;
            for (upstream_id, upstream_type) in match &specific {
                TaskStateSpecific::Empty(specific) => &specific.spec.upstream,
                TaskStateSpecific::Long(specific) => &specific.spec.upstream,
                TaskStateSpecific::Short(specific) => &specific.spec.upstream,
            } {
                match *upstream_type {
                    interface::task::DependencyType::Strong => { },
                    interface::task::DependencyType::Weak => {
                        if !is_control_effective_on(&get_task(state_dynamic, upstream_id)) {
                            all_on = false;
                            break;
                        }
                    },
                }
            }
            all_on
        }),
        actual: Cell::new((Actual::Stopped, Utc::now())),
        downstream: Default::default(),
        specific: specific,
        started_waiters: Default::default(),
        stopped_waiters: Default::default(),
        delete_when_stopped: Cell::new(delete_when_stopped),
    });
    state_dynamic.tasks.insert(task_id.clone(), task);
    {
        let task = &state_dynamic.task_alloc[task];
        let sender = state_dynamic.watchers_send.borrow_mut().take();
        if let Some(sender) = sender {
            for ev in [
                //. .
                Event {
                    task: task_id.clone(),
                    event: EventType::DirectOn(task.direct_on.get().0),
                },
                Event {
                    task: task_id.clone(),
                    event: EventType::TransitiveOn(task.transitive_on.get().0),
                },
                Event {
                    task: task_id.clone(),
                    event: EventType::EffectiveOn(is_control_effective_on(task)),
                },
            ] {
                _ = sender.send(ev) as Result<_, _>;
            }
            *state_dynamic.watchers_send.borrow_mut() = Some(sender);
        }
    }
}

pub(crate) fn delete_task_immediate(state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    // Remove task
    let task = state_dynamic.tasks.remove(task_id).unwrap();
    let task = state_dynamic.task_alloc.remove(task).unwrap();

    // Remove downstream entries from upstreams
    walk_task_upstream(&task, |upstream| {
        for (upstream_id, _) in upstream {
            let upstream = get_task(&state_dynamic, upstream_id);
            let mut downstream = upstream.downstream.borrow_mut();
            downstream.remove(task_id);
        }
    });

    // Remove schedulings
    let mut modified = false;
    state_dynamic.schedule.retain(|_, v| {
        v.retain(|r| {
            let ScheduleEvent::Rule(r) = r else {
                return true;
            };
            let keep = r.0 != *task_id;
            if !keep {
                modified = true;
            }
            return keep;
        });
        return !v.is_empty();
    });
    if modified {
        state_dynamic.notify_reschedule.notify_one();
    }
}

pub(crate) fn delete_task_recursive_off(
    state: &Arc<State>,
    state_dynamic: &mut StateDynamic,
    task: &TaskId,
    off: bool,
) {
    struct Entry {
        first: bool,
        task_id: TaskId,
    }

    let mut frontier = vec![Entry {
        first: true,
        task_id: task.clone(),
    }];
    while let Some(e) = frontier.pop() {
        if e.first {
            let Some(task) = maybe_get_task(&state_dynamic, &task) else {
                return;
            };
            frontier.push(Entry {
                first: false,
                task_id: e.task_id.clone(),
            });
            for (down_id, _down_type) in task.downstream.borrow().iter() {
                frontier.push(Entry {
                    first: true,
                    task_id: down_id.clone(),
                });
            }
        } else {
            if maybe_get_task(&state_dynamic, &e.task_id).is_some() && off {
                set_task_direct_off(&state, &mut *state_dynamic, &e.task_id);
            }
            if let Some(task) = maybe_get_task(&state_dynamic, &e.task_id) {
                if task.actual.get().0 == Actual::Stopped {
                    delete_task_immediate(state_dynamic, &e.task_id);
                } else {
                    task.delete_when_stopped.set(true);
                }
            }
        }
    }
    return ();
}
