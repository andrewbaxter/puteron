use {
    super::task_util::actual_set,
    crate::{
        demon::{
            interface::{
                base::TaskId,
                ipc::Actual,
                task::DependencyType,
            },
            state::{
                StateDynamic,
                TaskStateSpecific,
                TaskState_,
            },
            task_util::{
                are_all_downstream_tasks_stopped,
                are_all_strong_downstream_direct_or_transitive_off,
                are_all_weak_upstream_effective_on,
                awueo_set,
                direct_on_set,
                get_task,
                is_actual_all_upstream_tasks_started,
                is_control_effective_on,
                transitive_on_set,
                walk_task_upstream,
            },
        },
        interface::task::ShortTaskStartedAction,
    },
    std::collections::HashSet,
};

#[derive(Default, Debug)]
pub(crate) struct ExecutePlan {
    // For processless (instant transition) tasks
    pub(crate) log_starting: HashSet<TaskId>,
    pub(crate) log_started: HashSet<TaskId>,
    pub(crate) log_stopping: HashSet<TaskId>,
    pub(crate) log_stopped: HashSet<TaskId>,
    pub(crate) run: HashSet<TaskId>,
    pub(crate) stop: HashSet<TaskId>,
    pub(crate) delete: HashSet<TaskId>,
}

/// After state change
pub(crate) fn plan_event_started(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    plan.log_started.insert(task_id.clone());
    sync_actual_should_start_downstream_exclusive(state_dynamic, plan, task_id);
}

/// After state change
pub(crate) fn plan_event_stopping(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    plan.log_stopping.insert(task_id.clone());

    // Stop all downstream immediately
    let mut frontier = vec![];
    frontier.extend(get_task(state_dynamic, task_id).downstream.borrow().keys().cloned());
    while let Some(upstream_id) = frontier.pop() {
        let upstream_task = get_task(state_dynamic, &upstream_id);
        plan_actual_stop_one(state_dynamic, plan, &upstream_task, true);
        frontier.extend(upstream_task.downstream.borrow().keys().cloned());
    }
}

/// After state change
pub(crate) fn plan_event_stopped(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    sync_actual_should_stop_related(state_dynamic, plan, task_id);
}

/// Return true if started - downstream can be started now.
pub(crate) fn plan_actual_start_one(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task: &TaskState_) -> bool {
    if !is_control_effective_on(task) {
        return false;
    }
    if !is_actual_all_upstream_tasks_started(state_dynamic, task) {
        return false;
    }
    match task.actual.get().0 {
        Actual::Started => {
            return true;
        },
        Actual::Starting | Actual::Stopping => {
            return false;
        },
        Actual::Stopped => { },
    }
    match &task.specific {
        TaskStateSpecific::Empty(_) => {
            plan.log_starting.insert(task.id.clone());
            actual_set(state_dynamic, task, Actual::Starting);
            plan.log_started.insert(task.id.clone());
            actual_set(state_dynamic, task, Actual::Started);
            return true;
        },
        TaskStateSpecific::Long(_) => {
            if task.actual.get().0 != Actual::Stopped {
                return false;
            }
            plan.run.insert(task.id.clone());
            return false;
        },
        TaskStateSpecific::Short(_) => {
            if task.actual.get().0 != Actual::Stopped {
                return false;
            }
            plan.run.insert(task.id.clone());
            return false;
        },
    }
}

/// Return true if task is finished stopping (can continue with upstream).
pub(crate) fn plan_actual_stop_one(
    state_dynamic: &StateDynamic,
    plan: &mut ExecutePlan,
    task: &TaskState_,
    force: bool,
) {
    if !force && !are_all_downstream_tasks_stopped(state_dynamic, &task) {
        return;
    }
    if !force && is_control_effective_on(task) && is_actual_all_upstream_tasks_started(state_dynamic, task) {
        return;
    }
    if task.actual.get().0 == Actual::Stopped {
        return;
    }
    match &task.specific {
        TaskStateSpecific::Empty(_) => {
            plan.log_stopping.insert(task.id.clone());
            actual_set(state_dynamic, task, Actual::Stopping);
            plan.log_stopped.insert(task.id.clone());
            actual_set(state_dynamic, task, Actual::Stopped);
        },
        TaskStateSpecific::Long(_) => {
            plan.stop.insert(task.id.clone());
        },
        TaskStateSpecific::Short(specific) => {
            if task.actual.get().0 == Actual::Started {
                plan.log_stopping.insert(task.id.clone());
                actual_set(state_dynamic, task, Actual::Stopping);
                plan.log_stopped.insert(task.id.clone());
                actual_set(state_dynamic, task, Actual::Stopped);
                if let Some(ShortTaskStartedAction::Delete) = specific.spec.started_action {
                    plan.delete.insert(task.id.clone());
                }
            } else {
                plan.stop.insert(task.id.clone());
            }
        },
    }
}

pub(crate) fn plan_set_direct(
    state_dynamic: &StateDynamic,
    root_task_id: &TaskId,
    want_on: bool,
    // Called for each task whose state was modified, depth first.
    mut changed_cb: impl FnMut(&TaskState_),
) {
    // # Inclusive upward strong - adjust transitive_on
    let mut upstream_frontier = vec![(true, root_task_id.clone())];
    while let Some((first_pass, upstream_id)) = upstream_frontier.pop() {
        if first_pass {
            let upstream_task = get_task(state_dynamic, &upstream_id);

            // Update state, track if changed
            let was_on = upstream_task.direct_on.get().0 || upstream_task.transitive_on.get().0;
            if &upstream_id == root_task_id {
                if upstream_task.direct_on.get().0 != want_on {
                    direct_on_set(state_dynamic, upstream_task, want_on);
                }
            } else {
                if !want_on && !are_all_strong_downstream_direct_or_transitive_off(state_dynamic, upstream_task) {
                    // Can't change, no change
                    continue;
                }
                if upstream_task.transitive_on.get().0 != want_on {
                    transitive_on_set(state_dynamic, upstream_task, want_on);
                }
            }

            // If no change, abort ascent
            if was_on == want_on {
                // No change, abort upwards propagation
                continue;
            }

            // Ascend
            upstream_frontier.push((false, upstream_id));
            walk_task_upstream(&upstream_task, |upstream| {
                for (upstream_id, upstream_type) in upstream {
                    if *upstream_type != DependencyType::Strong {
                        continue;
                    }
                    upstream_frontier.push((true, upstream_id.clone()));
                }
            });
        } else {
            let upstream_task = get_task(state_dynamic, &upstream_id);

            // On changed, do cb
            changed_cb(upstream_task);

            // # Exclusive downward weak - adjust awueo/effective_on through weak downstreams
            if is_control_effective_on(upstream_task) == want_on {
                fn push_weak_downstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
                    for (downstream_id, downstream_type) in task.downstream.borrow().iter() {
                        if *downstream_type != DependencyType::Weak {
                            continue;
                        }
                        frontier.push(downstream_id.clone());
                    }
                }

                let mut downstream_frontier = vec![];
                push_weak_downstream(&mut downstream_frontier, upstream_task);
                while let Some(downstream_id) = downstream_frontier.pop() {
                    let downstream_task = get_task(state_dynamic, &downstream_id);
                    if downstream_task.awueo.get() == want_on {
                        // No change
                        continue;
                    }
                    if want_on && !are_all_weak_upstream_effective_on(state_dynamic, downstream_task) {
                        // Can't change, no change
                        continue;
                    }

                    // Update state
                    awueo_set(state_dynamic, downstream_task, want_on);

                    // Awueo changed, do cb
                    changed_cb(downstream_task);

                    // Descend
                    push_weak_downstream(&mut downstream_frontier, downstream_task);
                }
            }
        }
    }
}

pub(crate) fn sync_actual_should_start_downstream(
    state_dynamic: &StateDynamic,
    plan: &mut ExecutePlan,
    root_task_id: &TaskId,
    task: &TaskState_,
) {
    if plan_actual_start_one(state_dynamic, plan, task) {
        sync_actual_should_start_downstream_exclusive(state_dynamic, plan, root_task_id);
    }
}

pub(crate) fn plan_set_direct_on(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, root_task_id: &TaskId) {
    plan_set_direct(state_dynamic, root_task_id, true, |task| {
        plan_actual_start_one(state_dynamic, plan, task);
    });
    sync_actual_should_start_downstream(state_dynamic, plan, root_task_id, get_task(state_dynamic, root_task_id));
}

pub(crate) fn plan_set_direct_off(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    plan_set_direct(state_dynamic, task_id, false, |_task| { });
    sync_actual_should_stop_related(state_dynamic, plan, task_id);
}

fn sync_actual_should_start_downstream_exclusive(
    state_dynamic: &StateDynamic,
    plan: &mut ExecutePlan,
    from_task_id: &TaskId,
) {
    let mut frontier = vec![];

    fn push_downstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
        frontier.extend(task.downstream.borrow().keys().cloned());
    }

    push_downstream(&mut frontier, get_task(state_dynamic, from_task_id));
    while let Some(downstream_id) = frontier.pop() {
        let downstream = get_task(state_dynamic, &downstream_id);
        if !is_control_effective_on(&downstream) {
            continue;
        }
        if !plan_actual_start_one(state_dynamic, plan, &downstream) {
            continue;
        }
        push_downstream(&mut frontier, downstream);
    }
}

pub(crate) fn sync_actual_should_stop_related(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    let mut seen = HashSet::new();
    let mut upstream_frontier = vec![task_id.clone()];
    while let Some(upstream_id) = upstream_frontier.pop() {
        if seen.contains(&upstream_id) {
            continue;
        }
        let upstream_task = get_task(state_dynamic, &upstream_id);
        if is_control_effective_on(upstream_task) {
            continue;
        }

        // Stop leaves of weak downstream and self
        {
            let mut downstream_frontier = vec![(true, upstream_id.clone())];
            while let Some((first_pass, downstream_id)) = downstream_frontier.pop() {
                if seen.contains(&downstream_id) {
                    continue;
                }
                if first_pass {
                    let downstream_task = get_task(state_dynamic, &downstream_id);
                    downstream_frontier.push((false, downstream_id.clone()));
                    for (k, _) in downstream_task.downstream.borrow().iter() {
                        downstream_frontier.push((true, k.clone()));
                    }
                } else {
                    let downstream_task = get_task(state_dynamic, &downstream_id);
                    plan_actual_stop_one(state_dynamic, plan, &downstream_task, false);
                    seen.insert(downstream_id.clone());
                }
            }
        }

        // Subtree fully stopped, move upstream
        walk_task_upstream(upstream_task, |upstream| {
            for (up_id, _) in upstream {
                upstream_frontier.push(up_id.clone());
            }
        });
    }
}

/// Check:
///
/// * awueo
///
/// * transitive_on
///
/// * actual state
pub(crate) fn sanity_check(log: &loga::Log, state_dynamic: &StateDynamic, plan: &ExecutePlan) {
    let mut errors = vec![];

    // # Check transitive_on
    //
    // This must be done in graph order so that transitive values are correct.
    {
        let mut done = HashSet::new();
        loop {
            let initial_done_count = done.len();
            for (task_id, task) in &state_dynamic.tasks {
                if done.contains(task_id) {
                    continue;
                }
                let task = state_dynamic.task_alloc.get(*task).unwrap();
                let mut known = true;
                let mut downstream_on = HashSet::new();
                for (downstream_id, downstream_type) in task.downstream.borrow().iter() {
                    if *downstream_type != DependencyType::Strong {
                        continue;
                    };
                    if !done.contains(downstream_id) {
                        known = false;
                        break;
                    }
                    let downstream = get_task(state_dynamic, downstream_id);
                    if downstream.direct_on.get().0 || downstream.transitive_on.get().0 {
                        downstream_on.insert(downstream_id.clone());
                    }
                }
                if !known {
                    continue;
                }
                let want_transitive_on = !downstream_on.is_empty();
                if task.transitive_on.get().0 != want_transitive_on {
                    errors.push(
                        format!(
                            "Task [{}] transitive_on mismatch due to {:?}: want {} have {}",
                            task_id,
                            downstream_on,
                            want_transitive_on,
                            task.transitive_on.get().0
                        ),
                    );
                }
                done.insert(task_id);
            }
            if done.len() == initial_done_count {
                break;
            }
        }
    }

    // Check awueo
    {
        let mut done = HashSet::new();
        loop {
            let initial_done_count = done.len();
            for (task_id, task) in &state_dynamic.tasks {
                if done.contains(task_id) {
                    continue;
                }
                let task = state_dynamic.task_alloc.get(*task).unwrap();
                let mut known = true;
                let mut upstream_off = HashSet::new();
                walk_task_upstream(task, |upstream| {
                    for (upstream_id, upstream_type) in upstream {
                        if *upstream_type != DependencyType::Weak {
                            continue;
                        };
                        if !done.contains(upstream_id) {
                            known = false;
                            break;
                        }
                        let upstream = get_task(state_dynamic, upstream_id);
                        if !is_control_effective_on(upstream) {
                            upstream_off.insert(upstream_id.clone());
                        }
                    }
                });
                let want_awueo = upstream_off.is_empty();
                if known {
                    if task.awueo.get() != want_awueo {
                        errors.push(
                            format!(
                                "Task [{}] awueo mismatch due to {:?}: want {} have {}",
                                task_id,
                                upstream_off,
                                want_awueo,
                                task.awueo.get()
                            ),
                        );
                    }
                    done.insert(task_id);
                }
            }
            if done.len() == initial_done_count {
                break;
            }
        }
    }

    // Check actual/plan
    for (task_id, task) in &state_dynamic.tasks {
        let task = state_dynamic.task_alloc.get(*task).unwrap();
        let mut start = false;
        let mut stop = false;
        match task.actual.get().0 {
            Actual::Stopped => {
                if plan.run.contains(task_id) {
                    start = true;
                } else {
                    stop = true;
                }
            },
            Actual::Starting => {
                start = true;
                stop = true;
            },
            Actual::Started => {
                if plan.stop.contains(task_id) {
                    stop = true;
                } else {
                    start = true;
                }
            },
            Actual::Stopping => {
                start = true;
                stop = true;
            },
        }
        let mut is_actual_all_upstream_started2 = is_actual_all_upstream_tasks_started(state_dynamic, task);
        walk_task_upstream(task, |upstream| {
            for (upstream_id, _) in upstream {
                if plan.stop.contains(upstream_id) {
                    is_actual_all_upstream_started2 = false;
                }
            }
        });
        if is_control_effective_on(task) && is_actual_all_upstream_started2 && !start {
            errors.push(format!("Task [{}] on + all upstream started, but not starting", task_id));
        }
        let mut no_downstream_not_stopped = true;
        for (downstream_id, _downstream_type) in task.downstream.borrow().iter() {
            let downstream = get_task(state_dynamic, downstream_id);
            if downstream.actual.get().0 != Actual::Stopped {
                no_downstream_not_stopped = false;
                break;
            }
        }
        if !is_control_effective_on(task) && no_downstream_not_stopped && !stop {
            errors.push(format!("Task [{}] off and no downstream running but not stopping", task_id));
        }
        if !is_actual_all_upstream_started2 && !stop {
            let mut not_running = HashSet::new();
            walk_task_upstream(task, |upstream| {
                for (upstream_id, _upstream_type) in upstream {
                    let upstream = get_task(state_dynamic, upstream_id);
                    if upstream.actual.get().0 != Actual::Started {
                        not_running.insert(upstream_id.clone());
                    }
                }
            });
            errors.push(format!("Task [{}] upstream not running {:?} but not stopping", task_id, not_running));
        }
    }
    if !errors.is_empty() {
        log.log_err(
            loga::WARN,
            loga::agg_err("State constraint check failed", errors.into_iter().map(loga::err).collect::<Vec<_>>()),
        );
    }
}
