use {
    super::{
        base::TaskId,
        task::{
            schedule,
            DependencyType,
            Task,
        },
    },
    chrono::{
        DateTime,
        Utc,
    },
    glove::reqresp,
    schemars::JsonSchema,
    serde::{
        Deserialize,
        Serialize,
    },
    std::{
        collections::HashMap,
        env,
        path::PathBuf,
    },
};

/// Get the default ipc socket path.  Tries environment variable `PUTERON_IPC_SOCK`
/// for a specific pathname, then `puteron.sock` in `XDG_RUNTIME_DIR`, then
/// `/run/puteron.sock`. Because of this, when running as root it'll use a global
/// location, when as a user it'll use the user's session run directory.
pub fn ipc_path() -> Option<PathBuf> {
    if let Ok(p) = env::var("PUTERON_IPC_SOCK") {
        return Some(PathBuf::from(p));
    }
    return Some(PathBuf::from("/run/puteron.sock"));
}

// # Task
//
// List
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskList;

// Add
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskAdd {
    pub task: TaskId,
    pub spec: Task,
    /// Error if task already exists.
    pub unique: bool,
}

// On/off
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskOnOff {
    pub task: TaskId,
    pub on: bool,
}

// Delete
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskDelete(pub TaskId);

// Status
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskGetStatus(pub TaskId);

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStatusSpecificEmpty {}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Actual {
    Stopped,
    Starting,
    Started,
    Stopping,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStatusSpecificLong {
    pub pid: Option<i32>,
    pub restarts: usize,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStatusSpecificShort {
    pub pid: Option<i32>,
    pub restarts: usize,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskStatusSpecific {
    Empty(TaskStatusSpecificEmpty),
    Long(TaskStatusSpecificLong),
    Short(TaskStatusSpecificShort),
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStatus {
    pub direct_on: bool,
    pub direct_on_at: DateTime<Utc>,
    pub transitive_on: bool,
    pub transitive_on_at: DateTime<Utc>,
    pub effective_on: bool,
    pub actual: Actual,
    pub actual_at: DateTime<Utc>,
    pub specific: TaskStatusSpecific,
}

// Get spec
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskGetSpec(pub TaskId);

// Wait started
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskWaitStarted(pub TaskId);

// Wait stopped
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskWaitStopped(pub TaskId);

// List user-on
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskListUserOn;

// List upstream
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskUpstreamStatus {
    pub effective_on: bool,
    pub actual: Actual,
    pub dependency_type: DependencyType,
    pub upstream: HashMap<TaskId, TaskUpstreamStatus>,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskListUpstream {
    pub task: TaskId,
    pub include_started: bool,
}

// List downstream
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskDownstreamStatus {
    pub effective_on: bool,
    pub actual: Actual,
    pub dependency_type: DependencyType,
    pub effective_dependency_type: DependencyType,
    pub downstream: HashMap<TaskId, TaskDownstreamStatus>,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskListDownstream {
    pub task: TaskId,
    pub include_weak: bool,
    pub include_stopped: bool,
}

// # Demon
//
// Effective environment
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestDemonEnv;

// Schedule
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestDemonListSchedule;

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RespScheduleEntry {
    pub at: DateTime<Utc>,
    pub task: TaskId,
    pub rule: schedule::Rule,
}

// Spec dirs
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestDemonSpecDirs {}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EventType {
    DirectOn(bool),
    Actual(Actual),
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Event {
    pub task: TaskId,
    pub event_type: EventType,
}
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ReqTaskWatch;

reqresp!(pub ipc {
    TaskList(RequestTaskList) => Vec < TaskId >,
    TaskAdd(RequestTaskAdd) =>(),
    TaskDelete(RequestTaskDelete) =>(),
    TaskGetStatus(RequestTaskGetStatus) => TaskStatus,
    TaskGetSpec(RequestTaskGetSpec) => Task,
    TaskOnOff(RequestTaskOnOff) =>(),
    TaskWaitRunning(RequestTaskWaitStarted) =>(),
    TaskWaitStopped(RequestTaskWaitStopped) =>(),
    TaskListUserOn(RequestTaskListUserOn) => Vec < TaskId >,
    TaskListUpstream(RequestTaskListUpstream) => HashMap < TaskId,
    TaskUpstreamStatus >,
    TaskListDownstream(RequestTaskListDownstream) => HashMap < TaskId,
    TaskDownstreamStatus >,
    TaskWatch(ReqTaskWatch) => Vec < Event >,
    DemonEnv(RequestDemonEnv) => HashMap < String,
    String >,
    DemonListSchedule(RequestDemonListSchedule) => Vec < RespScheduleEntry >,
    DemonSpecDirs(RequestDemonSpecDirs) => Vec < PathBuf >,
});
