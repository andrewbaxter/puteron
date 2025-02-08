use {
    super::{
        state::{
            StateDynamic,
            TaskState_,
        },
        task_util::are_all_downstream_tasks_stopped,
    },
    crate::demon::{
        state::TaskStateSpecific,
        task_util::{
            are_all_upstream_tasks_started,
            are_all_weak_upstream_effective_on,
            get_task,
            is_task_effective_on,
            is_task_on,
            walk_task_upstream,
        },
        interface::{
            base::TaskId,
            ipc::Actual,
            task::DependencyType,
        },
    },
    chrono::Utc,
    std::collections::HashSet,
};

#[derive(Default, Debug)]
pub(crate) struct ExecutePlan {
    // For processless (instant transition) tasks
    pub(crate) log_started: HashSet<TaskId>,
    pub(crate) log_stopping: HashSet<TaskId>,
    pub(crate) log_stopped: HashSet<TaskId>,
    pub(crate) run: HashSet<TaskId>,
    pub(crate) stop: HashSet<TaskId>,
}

/// After state change
pub(crate) fn plan_event_started(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    plan.log_started.insert(task_id.clone());
    propagate_start_downstream(state_dynamic, plan, task_id);
}

/// After state change
pub(crate) fn plan_event_stopping(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    plan.log_stopping.insert(task_id.clone());

    // Stop all downstream immediately
    let mut frontier = vec![];
    frontier.extend(get_task(state_dynamic, task_id).downstream.borrow().keys().cloned());
    while let Some(upstream_id) = frontier.pop() {
        let upstream_task = get_task(state_dynamic, &upstream_id);
        plan_stop_one_task(state_dynamic, plan, &upstream_task);
        frontier.extend(upstream_task.downstream.borrow().keys().cloned());
    }
}

/// After state change
pub(crate) fn plan_event_stopped(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    propagate_stop(state_dynamic, plan, task_id);
}

/// Return true if started - downstream can be started now.
pub(crate) fn plan_start_one_task(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task: &TaskState_) -> bool {
    if !are_all_upstream_tasks_started(&state_dynamic, task) {
        return false;
    }
    match task.actual.get().0 {
        Actual::Started => return true,
        Actual::Starting | Actual::Stopping => return false,
        Actual::Stopped => { },
    }
    match &task.specific {
        TaskStateSpecific::Empty(_) => {
            task.actual.set((Actual::Started, Utc::now()));
            plan.log_started.insert(task.id.clone());
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
pub(crate) fn plan_stop_one_task(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task: &TaskState_) {
    if !are_all_downstream_tasks_stopped(state_dynamic, &task) {
        return;
    }
    if task.actual.get().0 == Actual::Stopped {
        return;
    }
    match &task.specific {
        TaskStateSpecific::Empty(_) => {
            task.actual.set((Actual::Stopped, Utc::now()));
            plan.log_stopped.insert(task.id.clone());
        },
        TaskStateSpecific::Long(_) => {
            plan.stop.insert(task.id.clone());
        },
        TaskStateSpecific::Short(_) => {
            if task.actual.get().0 == Actual::Started {
                plan.log_stopping.insert(task.id.clone());
                task.actual.set((Actual::Stopped, Utc::now()));
            } else {
                plan.stop.insert(task.id.clone());
            }
        },
    }
}

pub(crate) fn adjust_downstream_awueo(
    state_dynamic: &StateDynamic,
    root_task: &TaskState_,
    want_on: bool,
    changed_cb: &mut impl FnMut(&StateDynamic, &TaskState_),
) {
    fn push_weak_downstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
        for (downstream_id, downstream_type) in task.downstream.borrow().iter() {
            if *downstream_type != DependencyType::Weak {
                continue;
            }
            frontier.push(downstream_id.clone());
        }
    }

    let mut downstream_frontier = vec![];
    push_weak_downstream(&mut downstream_frontier, root_task);
    while let Some(downstream_id) = downstream_frontier.pop() {
        let downstream_task = get_task(state_dynamic, &downstream_id);
        if want_on {
            if downstream_task.awueo.get() {
                // No change
                continue;
            }
            if !are_all_weak_upstream_effective_on(state_dynamic, downstream_task) {
                // No change
                continue;
            }

            // Update all_weak_upstream_effective_on
            downstream_task.awueo.set(true);
        } else {
            if !downstream_task.awueo.get() {
                // No change
                continue;
            }
            downstream_task.awueo.set(false);
        }

        // State changed, continue downstream
        push_weak_downstream(&mut downstream_frontier, downstream_task);

        // State changed, call cb
        changed_cb(state_dynamic, downstream_task);
    }
}

// Adjusts `transitive_on` and `all_weak_upstream_effective_on` of the supertree
// of the specified task.
pub(crate) fn adjust_related_on(
    state_dynamic: &StateDynamic,
    root_task_id: &TaskId,
    want_on: bool,
    // Called for each task whose state was modified, depth first.
    mut changed_cb: impl FnMut(&StateDynamic, &TaskState_),
) {
    let mut seen = HashSet::new();
    let mut upstream_frontier = vec![(true, root_task_id.clone())];
    while let Some((first_pass, upstream_id)) = upstream_frontier.pop() {
        if seen.contains(&upstream_id) {
            continue;
        }
        if first_pass {
            let upstream_task = get_task(state_dynamic, &upstream_id);
            let was_on = is_task_on(&upstream_task);
            if &upstream_id != root_task_id {
                // Adjust transitive_on
                if upstream_task.transitive_on.get().0 != want_on {
                    upstream_task.transitive_on.set((want_on, Utc::now()));
                }
                if was_on == want_on {
                    // No state change, no need to visit sub/supertree
                    continue;
                }
            }
            upstream_frontier.push((false, upstream_id));

            // Continue upstream
            walk_task_upstream(&upstream_task, |upstream| {
                for (upstream_id, upstream_type) in upstream {
                    if *upstream_type != DependencyType::Strong {
                        continue;
                    }
                    upstream_frontier.push((true, upstream_id.clone()));
                }
            });
        } else {
            seen.insert(upstream_id.clone());
            let upstream_task = get_task(state_dynamic, &upstream_id);

            // transitive_on changed, call cb
            changed_cb(state_dynamic, upstream_task);

            // Adjust effective_on of subtree
            adjust_downstream_awueo(state_dynamic, upstream_task, want_on, &mut changed_cb);
        }
    }
}

pub(crate) fn plan_set_task_direct_on(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, root_task_id: &TaskId) {
    // Update on flags and check if the effective `on` state has changed
    let task = get_task(state_dynamic, root_task_id);
    let was_on = is_task_effective_on(&task);
    task.direct_on.set((true, Utc::now()));
    if was_on {
        return;
    }

    // Update related on status, start tasks
    adjust_related_on(state_dynamic, root_task_id, true, |state_dynamic, task| {
        // Only weakly-related, state changed elements
        if are_all_upstream_tasks_started(state_dynamic, task) {
            plan_start_one_task(state_dynamic, plan, task);
        }
    });

    // Start this and the whole subree around this task (strong + weak)
    if !are_all_upstream_tasks_started(state_dynamic, task) {
        return;
    }
    if !plan_start_one_task(state_dynamic, plan, task) {
        return;
    }
    propagate_start_downstream(state_dynamic, plan, root_task_id);
}

pub(crate) fn plan_set_task_direct_off(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    let task = get_task(state_dynamic, &task_id);
    if task.direct_on.get().0 {
        task.direct_on.set((false, Utc::now()));
    }
    if task.transitive_on.get().0 {
        return;
    }

    // Update related status
    adjust_related_on(state_dynamic, task_id, false, |_state_dynamic, _task| { });

    // Trigger state changes
    propagate_stop(state_dynamic, plan, task_id);
}

// When a task starts, start the next dependent downstream tasks
fn propagate_start_downstream(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, from_task_id: &TaskId) {
    let mut frontier = vec![];

    fn push_downstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
        frontier.extend(task.downstream.borrow().keys().cloned());
    }

    push_downstream(&mut frontier, get_task(state_dynamic, from_task_id));
    while let Some(downstream_id) = frontier.pop() {
        let downstream = get_task(state_dynamic, &downstream_id);
        if !is_task_on(&downstream) {
            continue;
        }
        if !plan_start_one_task(state_dynamic, plan, &downstream) {
            continue;
        }
        push_downstream(&mut frontier, downstream);
    }
}

// Stop (a task and) continue to all off upstreams, and all weak downstreams of
// those.
fn propagate_stop(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    let mut seen = HashSet::new();
    let mut upstream_frontier = vec![task_id.clone()];
    while let Some(upstream_id) = upstream_frontier.pop() {
        if seen.contains(&upstream_id) {
            continue;
        }
        let upstream_task = get_task(state_dynamic, &upstream_id);
        if is_task_effective_on(upstream_task) {
            continue;
        }

        // Stop all weak downstream and task itself
        {
            let mut downstream_frontier = vec![(true, upstream_id.clone(), DependencyType::Strong)];
            while let Some((first_pass, downstream_id, downstream_type)) = downstream_frontier.pop() {
                if seen.contains(&downstream_id) {
                    continue;
                }
                if first_pass {
                    let downstream_task = get_task(state_dynamic, &downstream_id);
                    match downstream_type {
                        DependencyType::Strong => {
                            if is_task_effective_on(&downstream_task) {
                                continue;
                            }
                        },
                        DependencyType::Weak => { },
                    }
                    downstream_frontier.push((false, downstream_id.clone(), downstream_type));

                    // Descend
                    for (k, v) in downstream_task.downstream.borrow().iter() {
                        downstream_frontier.push((true, k.clone(), *v));
                    }
                } else {
                    // Stop if possible
                    let downstream_task = get_task(state_dynamic, &downstream_id);
                    plan_stop_one_task(state_dynamic, plan, &downstream_task);
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
