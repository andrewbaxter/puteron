use {
    super::{
        state::{
            StateDynamic,
            TaskState_,
        },
        task_util::{
            are_all_downstream_tasks_stopped,
            is_task_on,
            is_task_stopped,
        },
    },
    crate::demon::{
        state::TaskStateSpecific,
        task_util::{
            are_all_upstream_tasks_started,
            get_task,
            is_task_started,
            walk_task_upstream,
        },
    },
    chrono::Utc,
    puteron_lib::interface::{
        base::TaskId,
        message::ProcState,
        task::DependencyType,
    },
    std::{
        collections::HashSet,
    },
};

#[derive(Default)]
pub(crate) struct ExecutePlan {
    pub(crate) log_starting: HashSet<TaskId>,
    pub(crate) log_stopping: HashSet<TaskId>,
    pub(crate) start: HashSet<TaskId>,
    pub(crate) stop: HashSet<TaskId>,
}

/// After state changes
fn plan_event_starting(plan: &mut ExecutePlan, task_id: &TaskId) {
    plan.log_starting.insert(task_id.clone());
}

/// After state change
pub(crate) fn plan_event_started(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
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
        plan_stop_task(state_dynamic, plan, &upstream_task);
        frontier.extend(upstream_task.downstream.borrow().keys().cloned());
    }
}

/// After state change
pub(crate) fn plan_event_stopped(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    propagate_stop_upstream(state_dynamic, plan, task_id);
}

/// Return true if started - downstream can be started now.
pub(crate) fn plan_start_task(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task: &TaskState_) -> bool {
    if !are_all_upstream_tasks_started(&state_dynamic, task) {
        return false;
    }
    if is_task_started(task) {
        return true;
    }
    match &task.specific {
        TaskStateSpecific::Empty(s) => {
            plan_event_starting(plan, &task.id);
            s.started.set((true, Utc::now()));
            plan_event_started(state_dynamic, plan, &task.id);
            return true;
        },
        TaskStateSpecific::Long(s) => {
            if s.state.get().0 != ProcState::Stopped {
                return false;
            }
            plan.start.insert(task.id.clone());
            return false;
        },
        TaskStateSpecific::Short(s) => {
            if s.state.get().0 != ProcState::Stopped {
                return false;
            }
            plan.start.insert(task.id.clone());
            return false;
        },
    }
}

/// Return true if task is finished stopping (can continue with upstream).
pub(crate) fn plan_stop_task(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task: &TaskState_) -> bool {
    if !are_all_downstream_tasks_stopped(state_dynamic, &task) {
        return false;
    }
    if is_task_stopped(task) {
        return true;
    }
    match &task.specific {
        TaskStateSpecific::Empty(specific) => {
            plan.log_stopping.insert(task.id.clone());
            specific.started.set((false, Utc::now()));
            plan_event_stopped(state_dynamic, plan, &task.id);
            return true;
        },
        TaskStateSpecific::Long(_) => {
            plan.stop.insert(task.id.clone());
            return false;
        },
        TaskStateSpecific::Short(specific) => {
            if specific.state.get().0 == ProcState::Started {
                plan.log_stopping.insert(task.id.clone());
                specific.state.set((ProcState::Stopped, Utc::now()));
            } else {
                plan.stop.insert(task.id.clone());
            }
            return false;
        },
    }
}

pub(crate) fn plan_set_task_direct_on(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, root_task_id: &TaskId) {
    // Update on flags and check if the effective `on` state has changed
    {
        let task = get_task(state_dynamic, root_task_id);
        let was_on = is_task_on(&task);
        task.direct_on.set((true, Utc::now()));
        if was_on {
            return;
        }

        // Set transitive_on for strong deps, start leaves
        {
            let mut frontier = vec![];

            fn push_frontier(frontier: &mut Vec<(bool, TaskId)>, task: &TaskState_) {
                walk_task_upstream(&task, |upstream| {
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
                    let was_on = is_task_on(&upstream_task);
                    upstream_task.transitive_on.set((true, Utc::now()));
                    if was_on {
                        continue;
                    }
                    frontier.push((false, upstream_id));
                    push_frontier(&mut frontier, upstream_task);
                } else {
                    let upstream_task = get_task(state_dynamic, &upstream_id);
                    if are_all_upstream_tasks_started(state_dynamic, &upstream_task) {
                        plan_start_task(state_dynamic, plan, &upstream_task);
                    }
                }
            }
        }

        // Start this
        if !are_all_upstream_tasks_started(state_dynamic, task) {
            return;
        }
        if !plan_start_task(state_dynamic, plan, task) {
            return;
        }
    }

    // If everything else has started, start downstream
    propagate_start_downstream(state_dynamic, plan, root_task_id);
}

pub(crate) fn plan_set_task_direct_off(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    // Update on flags and check if the effective `on` state has changed
    {
        let task = get_task(state_dynamic, &task_id);
        let was_off = !is_task_on(&task);
        if was_off {
            return;
        }
        task.direct_on.set((false, Utc::now()));
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

                fn can_stop_task(task: &TaskState_) -> bool {
                    if is_task_stopped(task) {
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
                            if specific.state.get().0 == ProcState::Started {
                                return true;
                            }
                            return specific.stop.borrow().is_some();
                        },
                    }
                }

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
                    if !plan_stop_task(state_dynamic, plan, &downstream_task) {
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
        propagate_stop_upstream(state_dynamic, plan, task_id);
    }
}

pub(crate) fn propagate_transitive_off(state_dynamic: &StateDynamic, task_id: &TaskId) {
    let mut frontier = vec![];

    fn push_upstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
        walk_task_upstream(&task, |upstream| {
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
        if !is_task_on(&upstream_task) {
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
            if is_task_on(get_task(state_dynamic, downstream_id)) {
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
        if !plan_start_task(state_dynamic, plan, &downstream) {
            continue;
        }
        push_downstream(&mut frontier, downstream);
    }
}

// When a task stops, stop the next upstream tasks that were started as
// dependencies
fn propagate_stop_upstream(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    let mut frontier = vec![];

    fn push_upstream(frontier: &mut Vec<TaskId>, task: &TaskState_) {
        walk_task_upstream(task, |upstream| {
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
        if is_task_on(upstream_task) {
            continue;
        }
        if !plan_stop_task(state_dynamic, plan, &upstream_task) {
            continue;
        }
        push_upstream(&mut frontier, &upstream_task);
    }
}
