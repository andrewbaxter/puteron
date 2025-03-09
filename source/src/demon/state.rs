use {
    super::schedule::{
        ScheduleDynamic,
        ScheduleEvent,
    },
    crate::interface::{
        self,
        base::TaskId,
        ipc::{
            Actual,
            Event,
        },
        task::DependencyType,
    },
    chrono::{
        DateTime,
        Utc,
    },
    loga::Log,
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
        sync::{
            Arc,
            Mutex,
        },
    },
    tokio::{
        sync::{
            broadcast,
            oneshot,
            Notify,
        },
        time::Instant,
    },
    tokio_util::{
        sync::CancellationToken,
        task::TaskTracker,
    },
};

pub(crate) struct TaskStateEmpty {
    pub(crate) spec: interface::task::TaskSpecEmpty,
}

pub(crate) struct TaskStateLong {
    pub(crate) pid: Cell<Option<i32>>,
    pub(crate) failed_start_count: Cell<usize>,
    pub(crate) stop: RefCell<Option<oneshot::Sender<()>>>,
    pub(crate) spec: interface::task::TaskSpecLong,
}

pub(crate) struct TaskStateShort {
    pub(crate) pid: Cell<Option<i32>>,
    pub(crate) failed_start_count: Cell<usize>,
    pub(crate) stop: RefCell<Option<oneshot::Sender<()>>>,
    pub(crate) spec: interface::task::TaskSpecShort,
}

pub(crate) enum TaskStateSpecific {
    Empty(TaskStateEmpty),
    Long(TaskStateLong),
    Short(TaskStateShort),
}

pub(crate) struct TaskState_ {
    pub(crate) id: TaskId,
    pub(crate) direct_on: Cell<(bool, DateTime<Utc>)>,
    pub(crate) transitive_on: Cell<(bool, DateTime<Utc>)>,
    // "all weak upstream effective on"
    pub(crate) awueo: Cell<bool>,
    pub(crate) actual: Cell<(Actual, DateTime<Utc>)>,
    pub(crate) downstream: RefCell<HashMap<TaskId, DependencyType>>,
    pub(crate) specific: TaskStateSpecific,
    pub(crate) started_waiters: RefCell<Vec<oneshot::Sender<bool>>>,
    pub(crate) stopped_waiters: RefCell<Vec<oneshot::Sender<bool>>>,
}

new_key_type!{
    pub(crate) struct TaskState;
}

pub(crate) struct StateDynamic {
    pub(crate) task_alloc: SlotMap<TaskState, TaskState_>,
    // Downstream tasks are guaranteed to exist. Upstream tasks may or may not exist.
    pub(crate) tasks: HashMap<TaskId, TaskState>,
    // For cli command only, stores the current rule being waited on
    pub(crate) schedule_top: Option<(Instant, ScheduleEvent)>,
    pub(crate) schedule: ScheduleDynamic,
    pub(crate) notify_reschedule: Arc<Notify>,
    pub(crate) watchers_send: RefCell<Option<broadcast::Sender<Event>>>,
    // pid -> queue handle
    pub(crate) idle_watchers: HashMap<i32, (Instant, broadcast::Receiver<Event>)>,
}

pub(crate) struct State {
    pub(crate) debug: bool,
    pub(crate) shutdown: CancellationToken,
    pub(crate) log: Log,
    pub(crate) task_dirs: Vec<PathBuf>,
    pub(crate) env: HashMap<String, String>,
    pub(crate) dynamic: Mutex<StateDynamic>,
    pub(crate) tokio_tasks: TaskTracker,
}
