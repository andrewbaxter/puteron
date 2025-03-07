use {
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
                are_all_weak_upstream_effective_on,
                get_task,
                is_actual_all_upstream_tasks_started,
                is_control_effective_on,
                walk_task_upstream,
            },
        },
        interface::task::ShortTaskStartedAction,
    },
    chrono::Utc,
    std::collections::{
        HashSet,
    },
};

#[derive(Default, Debug)]
pub(crate) struct ExecutePlan {
    // For processless (instant transition) tasks
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
        plan_actual_stop_one(state_dynamic, plan, &upstream_task);
        frontier.extend(upstream_task.downstream.borrow().keys().cloned());
    }
}

/// After state change
pub(crate) fn plan_event_stopped(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task_id: &TaskId) {
    eprintln!("plan event stopped {}--", task_id);
    sync_actual_should_stop_related(state_dynamic, plan, task_id);
    eprintln!("plan event stopped {}--done", task_id);
}

/// Return true if started - downstream can be started now.
pub(crate) fn plan_actual_start_one(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task: &TaskState_) -> bool {
    if !is_control_effective_on(task) {
        eprintln!("   --> not effective on");
        return false;
    }
    if !is_actual_all_upstream_tasks_started(state_dynamic, task) {
        eprintln!("   --> not all upstream started");
        return false;
    }
    match task.actual.get().0 {
        Actual::Started => {
            eprintln!("   --> already started");
            return true;
        },
        Actual::Starting | Actual::Stopping => {
            eprintln!("   --> starting or stopping");
            return false;
        },
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
pub(crate) fn plan_actual_stop_one(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, task: &TaskState_) {
    if !are_all_downstream_tasks_stopped(state_dynamic, &task) {
        return;
    }
    if is_control_effective_on(task) && is_actual_all_upstream_tasks_started(state_dynamic, task) {
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
        TaskStateSpecific::Short(specific) => {
            if task.actual.get().0 == Actual::Started {
                plan.log_stopping.insert(task.id.clone());
                task.actual.set((Actual::Stopped, Utc::now()));
                if let Some(ShortTaskStartedAction::Delete) = specific.spec.started_action {
                    eprintln!("plan stop one - delete {}", task.id);
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
    eprintln!("set direct on={} -- {}", want_on, root_task_id);

    // # Inclusive upward strong - adjust transitive_on
    let mut seen_upstream = HashSet::new();
    let mut upstream_frontier = vec![(true, root_task_id.clone())];
    while let Some((first_pass, upstream_id)) = upstream_frontier.pop() {
        eprintln!(" (weakly related) upstream {}; first {})", upstream_id, first_pass);
        if seen_upstream.contains(&upstream_id) {
            continue;
        }
        if first_pass {
            let upstream_task = get_task(state_dynamic, &upstream_id);

            // Update state, track if changed
            let was_on = upstream_task.direct_on.get().0 || upstream_task.transitive_on.get().0;
            if &upstream_id == root_task_id {
                if upstream_task.direct_on.get().0 != want_on {
                    upstream_task.direct_on.set((want_on, Utc::now()));
                }
            } else {
                if upstream_task.transitive_on.get().0 != want_on {
                    upstream_task.transitive_on.set((want_on, Utc::now()));
                }
            }

            // If no change, abort ascent
            if was_on == want_on {
                eprintln!(" --> already on = {}", want_on);

                // No change, abort upwards propagation
                seen_upstream.insert(upstream_id.clone());
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
            seen_upstream.insert(upstream_id.clone());
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
                    eprintln!(" (sync awueo down) {}", downstream_id);
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
                    downstream_task.awueo.set(want_on);
                    eprintln!("   (( awueo={} for {}", want_on, downstream_id);

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
    eprintln!(" (sync actual down) down {})", root_task_id);
    if plan_actual_start_one(state_dynamic, plan, task) {
        sync_actual_should_start_downstream_exclusive(state_dynamic, plan, root_task_id);
    }
}

pub(crate) fn plan_set_direct_on(state_dynamic: &StateDynamic, plan: &mut ExecutePlan, root_task_id: &TaskId) {
    plan_set_direct(state_dynamic, root_task_id, true, |task| {
        eprintln!(" -- start weakly related {}", task.id);
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
        eprintln!(" (sync actual down2) down {})", downstream_id);
        let downstream = get_task(state_dynamic, &downstream_id);
        if !is_control_effective_on(&downstream) {
            eprintln!("  --> not on");
            continue;
        }
        eprintln!("  --> starting");
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
        eprintln!("propagate stop, at upstream {}", upstream_id);
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
                    eprintln!(" --> downstream {}", downstream_id);
                    let downstream_task = get_task(state_dynamic, &downstream_id);
                    plan_actual_stop_one(state_dynamic, plan, &downstream_task);
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

    // Check transitive_on
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
                let mut want_transitive_on = false;
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
                        want_transitive_on = true;
                    }
                }
                if known {
                    if task.transitive_on.get().0 != want_transitive_on {
                        errors.push(
                            format!(
                                "Task [{}] transitive_on mismatch: want {} have {}",
                                task_id,
                                want_transitive_on,
                                task.transitive_on.get().0
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
                let mut want_awueo = true;
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
                            want_awueo = false;
                        }
                    }
                });
                if known {
                    if task.awueo.get() != want_awueo {
                        errors.push(
                            format!("Task [{}] awueo mismatch: want {} have {}", task_id, want_awueo, task.awueo.get()),
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
        if is_control_effective_on(task) && is_actual_all_upstream_tasks_started(state_dynamic, task) && !start {
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
        if (!is_actual_all_upstream_tasks_started(state_dynamic, task) ||
            (!is_control_effective_on(task) && no_downstream_not_stopped)) &&
            !stop {
            errors.push(
                format!("Task [{}] upstream not running or off and no downstream running, but not stopping", task_id),
            );
        }
    }
    if !errors.is_empty() {
        log.log_err(
            loga::WARN,
            loga::agg_err("State constraint check failed", errors.into_iter().map(loga::err).collect::<Vec<_>>()),
        );
    }
}
