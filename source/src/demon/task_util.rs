use {
    super::state::{
        StateDynamic,
        TaskStateSpecific,
        TaskState_,
    },
    crate::interface::{
        self,
        base::TaskId,
        ipc::Actual,
        task::{
            DependencyType,
            ShortTaskStartedAction,
            TaskSpecShort,
        },
    },
};

pub(crate) fn walk_task_upstream<
    'a,
    T,
>(t: &'a TaskState_, mut cb: impl FnMut(&mut dyn Iterator<Item = (&'a TaskId, &'a DependencyType)>) -> T) -> T {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return cb(&mut s.spec.upstream.iter()),
        TaskStateSpecific::Long(s) => return cb(&mut s.spec.upstream.iter()),
        TaskStateSpecific::Short(s) => return cb(&mut s.spec.upstream.iter()),
    }
}

pub(crate) fn get_task<'a>(state_dynamic: &'a StateDynamic, task_id: &TaskId) -> &'a TaskState_ {
    return &state_dynamic.task_alloc[*state_dynamic.tasks.get(task_id).unwrap()];
}

pub(crate) fn maybe_get_task<'a>(state_dynamic: &'a StateDynamic, task_id: &TaskId) -> Option<&'a TaskState_> {
    let Some(t) = state_dynamic.tasks.get(task_id) else {
        return None;
    };
    return Some(&state_dynamic.task_alloc[*t]);
}

/// Effective on = `"all weak upstream on" && ("direct_on" || "transitive_on")`
pub(crate) fn is_task_effective_on(t: &TaskState_) -> bool {
    return t.awueo.get() && is_task_on(t);
}

pub(crate) fn is_task_on(t: &TaskState_) -> bool {
    return t.direct_on.get().0 || t.transitive_on.get().0;
}

pub(crate) fn get_short_task_started_action(specific: &TaskSpecShort) -> ShortTaskStartedAction {
    return match specific.started_action {
        None => {
            if specific.schedule.is_empty() {
                interface::task::ShortTaskStartedAction::None
            } else {
                interface::task::ShortTaskStartedAction::TurnOff
            }
        },
        Some(a) => a,
    };
}

pub(crate) fn are_all_upstream_tasks_started(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    let mut all_started = true;
    walk_task_upstream(task, |deps| {
        for (task_id, _) in deps {
            let Some(dep) = state_dynamic.tasks.get(task_id) else {
                all_started = false;
                return;
            };
            if state_dynamic.task_alloc[*dep].actual.get().0 != Actual::Started {
                all_started = false;
                return;
            }
        }
    });
    return all_started;
}

pub(crate) fn are_all_downstream_tasks_stopped(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    for (task_id, _) in task.downstream.borrow().iter() {
        if get_task(state_dynamic, task_id).actual.get().0 != Actual::Stopped {
            return false;
        }
    }
    return true;
}

pub(crate) fn are_all_weak_upstream_effective_on(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    let mut all = true;
    walk_task_upstream(task, |upstream| {
        for (upstream_id, upstream_type) in upstream {
            if *upstream_type != DependencyType::Weak {
                continue;
            }
            let effective_on = is_task_effective_on(&get_task(state_dynamic, upstream_id));
            if !effective_on {
                eprintln!("are all - {} is not effective on", upstream_id);
                all = false;
                break;
            }
        }
    });
    return all;
}
