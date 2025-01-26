use {
    super::{
        schedule::{
            calc_next_instant,
            ScheduleRule,
        },
        state::{
            StateDynamic,
            TaskStateEmpty,
            TaskStateLong,
            TaskStateShort,
            TaskStateSpecific,
            TaskState_,
        },
        task_util::{
            get_task,
            maybe_get_task,
            walk_task_upstream,
        },
    },
    chrono::Utc,
    puteron_lib::interface::{
        self,
        base::TaskId,
        message::ProcState,
        task::Task,
    },
    std::cell::{
        Cell,
        RefCell,
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
                        interface::task::ShortTaskStartedAction::Delete => {
                            errors.push(
                                loga::err(
                                    format!(
                                        "Task [{}] upstream [{}] has started action delete so this task will never be able to start",
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

pub(crate) fn build_task(state_dynamic: &mut StateDynamic, task_id: TaskId, spec: Task) {
    let specific;
    match spec {
        interface::task::Task::Empty(spec) => {
            for (upstream_id, upstream_type) in &spec.upstream {
                get_task(state_dynamic, upstream_id)
                    .downstream
                    .borrow_mut()
                    .insert(task_id.clone(), upstream_type.clone());
            }
            specific = TaskStateSpecific::Empty(TaskStateEmpty {
                started: Cell::new((false, Utc::now())),
                spec: spec,
            });
        },
        interface::task::Task::Long(spec) => {
            for (upstream_id, upstream_type) in &spec.upstream {
                get_task(state_dynamic, &upstream_id)
                    .downstream
                    .borrow_mut()
                    .insert(task_id.clone(), upstream_type.clone());
            }
            specific = TaskStateSpecific::Long(TaskStateLong {
                spec: spec,
                state: Cell::new((ProcState::Stopped, Utc::now())),
                stop: RefCell::new(None),
                pid: Cell::new(None),
                failed_start_count: Cell::new(0),
            });
        },
        interface::task::Task::Short(spec) => {
            for rule in &spec.schedule {
                state_dynamic
                    .schedule
                    .entry(calc_next_instant(Utc::now(), Instant::now(), rule, true))
                    .or_default()
                    .push(ScheduleRule::new((task_id.clone(), rule.clone())));
            }
            state_dynamic.notify_reschedule.notify_one();
            for (upstream_id, upstream_type) in &spec.upstream {
                get_task(state_dynamic, &upstream_id)
                    .downstream
                    .borrow_mut()
                    .insert(task_id.clone(), upstream_type.clone());
            }
            specific = TaskStateSpecific::Short(TaskStateShort {
                spec: spec,
                state: Cell::new((ProcState::Stopped, Utc::now())),
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
        downstream: Default::default(),
        specific: specific,
        started_waiters: Default::default(),
        stopped_waiters: Default::default(),
    });
    state_dynamic.tasks.insert(task_id, task);
}

pub(crate) fn delete_task(state_dynamic: &mut StateDynamic, task_id: &TaskId) {
    // Remove task
    let task = state_dynamic.tasks.remove(task_id).unwrap();
    let task = state_dynamic.task_alloc.remove(task).unwrap();

    // Remove downstream entries
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
