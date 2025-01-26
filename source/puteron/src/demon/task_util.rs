use {
    super::state::{
        StateDynamic,
        TaskStateSpecific,
        TaskState_,
    },
    puteron_lib::interface::{
        self,
        base::TaskId,
        message::{
            ProcState,
        },
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

pub(crate) fn is_task_on(t: &TaskState_) -> bool {
    return t.direct_on.get().0 || t.transitive_on.get().0;
}

pub(crate) fn is_task_started(t: &TaskState_) -> bool {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return s.started.get().0,
        TaskStateSpecific::Long(s) => {
            return s.state.get().0 == ProcState::Started;
        },
        TaskStateSpecific::Short(s) => {
            return s.state.get().0 == ProcState::Started;
        },
    }
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
            if !is_task_started(&state_dynamic.task_alloc[*dep]) {
                all_started = false;
                return;
            }
        }
    });
    return all_started;
}

pub(crate) fn is_task_stopped(t: &TaskState_) -> bool {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return !s.started.get().0,
        TaskStateSpecific::Long(s) => {
            return s.state.get().0 == ProcState::Stopped;
        },
        TaskStateSpecific::Short(s) => {
            return s.state.get().0 == ProcState::Stopped;
        },
    }
}

pub(crate) fn are_all_downstream_tasks_stopped(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    for (task_id, _) in task.downstream.borrow().iter() {
        if !is_task_stopped(get_task(state_dynamic, task_id)) {
            return false;
        }
    }
    return true;
}
