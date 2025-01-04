use {
    chrono::{
        DateTime,
        Utc,
    },
    puteron_lib::interface::{
        self,
        base::TaskId,
        message::v1::ProcState,
        task::DependencyType,
    },
    slotmap::{
        new_key_type,
        SlotMap,
    },
    std::{
        cell::{
            Cell,
            RefCell,
        },
        collections::HashMap,
        path::PathBuf,
        sync::Mutex,
    },
    tokio::sync::oneshot,
};

pub(crate) struct TaskStateEmpty {
    pub(crate) started: Cell<(bool, DateTime<Utc>)>,
    pub(crate) spec: interface::task::TaskSpecEmpty,
}

pub(crate) struct TaskStateLong {
    pub(crate) state: Cell<(ProcState, DateTime<Utc>)>,
    pub(crate) pid: Cell<Option<i32>>,
    pub(crate) failed_start_count: Cell<usize>,
    pub(crate) stop: RefCell<Option<oneshot::Sender<()>>>,
    pub(crate) spec: interface::task::TaskSpecLong,
}

pub(crate) struct TaskStateShort {
    pub(crate) state: Cell<(ProcState, DateTime<Utc>)>,
    pub(crate) pid: Cell<Option<i32>>,
    pub(crate) failed_start_count: Cell<usize>,
    pub(crate) stop: RefCell<Option<oneshot::Sender<()>>>,
    pub(crate) spec: interface::task::TaskSpecShort,
}

pub(crate) enum TaskStateSpecific {
    Empty(TaskStateEmpty),
    Long(TaskStateLong),
    Short(TaskStateShort),
    External,
}

pub(crate) struct TaskState_ {
    pub(crate) id: TaskId,
    pub(crate) user_on: Cell<(bool, DateTime<Utc>)>,
    pub(crate) transitive_on: Cell<(bool, DateTime<Utc>)>,
    pub(crate) specific: TaskStateSpecific,
    pub(crate) started_waiters: RefCell<Vec<oneshot::Sender<bool>>>,
    pub(crate) stopped_waiters: RefCell<Vec<oneshot::Sender<bool>>>,
}

new_key_type!{
    pub(crate) struct TaskState;
}

pub(crate) struct StateDynamic {
    pub(crate) task_alloc: SlotMap<TaskState, TaskState_>,
    pub(crate) downstream: HashMap<TaskId, HashMap<TaskId, DependencyType>>,
    // Downstream tasks are guaranteed to exist. Upstream tasks may or may not exist.
    pub(crate) tasks: HashMap<TaskId, TaskState>,
}

pub(crate) struct State {
    pub(crate) task_dirs: Vec<PathBuf>,
    pub(crate) env: HashMap<String, String>,
    pub(crate) dynamic: Mutex<StateDynamic>,
}

pub(crate) fn task_started(t: &TaskState_) -> bool {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return s.started.get().0,
        TaskStateSpecific::Long(s) => {
            return s.state.get().0 == ProcState::Started;
        },
        TaskStateSpecific::Short(s) => {
            return s.state.get().0 == ProcState::Started;
        },
        TaskStateSpecific::External => unreachable!(),
    }
}

pub(crate) fn task_on(t: &TaskState_) -> bool {
    return t.user_on.get().0 || t.transitive_on.get().0;
}

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

pub(crate) fn all_upstream_tasks_started(state_dynamic: &StateDynamic, task: &TaskState_) -> bool {
    let mut all_started = true;
    upstream(task, |deps| {
        for (task_id, _) in deps {
            let Some(dep) = state_dynamic.tasks.get(task_id) else {
                all_started = false;
                return;
            };
            if !task_started(&state_dynamic.task_alloc[*dep]) {
                all_started = false;
                return;
            }
        }
    });
    return all_started;
}

pub(crate) fn task_stopped(t: &TaskState_) -> bool {
    match &t.specific {
        TaskStateSpecific::Empty(s) => return !s.started.get().0,
        TaskStateSpecific::Long(s) => {
            return s.state.get().0 == ProcState::Stopped;
        },
        TaskStateSpecific::Short(s) => {
            return s.state.get().0 == ProcState::Stopped;
        },
        TaskStateSpecific::External => unreachable!(),
    }
}
