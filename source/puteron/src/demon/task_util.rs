use {
    super::state::{
        StateDynamic,
        TaskStateSpecific,
        TaskState_,
    },
    puteron_lib::interface::{
        base::TaskId,
        task::DependencyType,
    },
};

pub(crate) fn upstream<
    'a,
    T,
>(t: &'a TaskState_, mut cb: impl FnMut(&mut dyn Iterator<Item = (&'a TaskId, &'a DependencyType)>) -> T) -> T {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return cb(&mut s.spec.upstream.iter()),
        TaskStateSpecific::Long(s) => return cb(&mut s.spec.upstream.iter()),
        TaskStateSpecific::Short(s) => return cb(&mut s.spec.upstream.iter()),
        TaskStateSpecific::External => return cb(&mut [].into_iter()),
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
