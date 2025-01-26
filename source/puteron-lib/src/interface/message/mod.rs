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
        path::PathBuf,
        collections::HashMap,
    },
};

// # Task
//
// Add
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestAdd {
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
pub struct TaskStatusSpecificEmpty {
    pub started: bool,
    pub started_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProcState {
    Stopped,
    Starting,
    Started,
    Stopping,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStatusSpecificLong {
    pub state: ProcState,
    pub state_at: DateTime<Utc>,
    pub pid: Option<i32>,
    pub restarts: usize,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStatusSpecificShort {
    pub state: ProcState,
    pub state_at: DateTime<Utc>,
    pub pid: Option<i32>,
    pub restarts: usize,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskStatusSpecific {
    Empty(TaskStatusSpecificEmpty),
    Long(TaskStatusSpecificLong),
    Short(TaskStatusSpecificShort),
    External,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskStatus {
    pub direct_on: bool,
    pub direct_on_at: DateTime<Utc>,
    pub transitive_on: bool,
    pub transitive_on_at: DateTime<Utc>,
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
pub struct TaskDependencyStatus {
    pub on: bool,
    pub started: bool,
    pub dependency_type: DependencyType,
    pub related: HashMap<TaskId, TaskDependencyStatus>,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskListUpstream(pub TaskId);

// List downstream
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskListDownstream(pub TaskId);

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

reqresp!(pub ipc {
    TaskAdd(RequestAdd) =>(),
    TaskDelete(RequestTaskDelete) =>(),
    TaskGetStatus(RequestTaskGetStatus) => TaskStatus,
    TaskGetSpec(RequestTaskGetSpec) => Task,
    TaskOnOff(RequestTaskOnOff) =>(),
    TaskWaitStarted(RequestTaskWaitStarted) =>(),
    TaskWaitStopped(RequestTaskWaitStopped) =>(),
    TaskListUserOn(RequestTaskListUserOn) => Vec < TaskId >,
    TaskListUpstream(RequestTaskListUpstream) => HashMap < TaskId,
    TaskDependencyStatus >,
    TaskListDownstream(RequestTaskListDownstream) => HashMap < TaskId,
    TaskDependencyStatus >,
    DemonEnv(RequestDemonEnv) => HashMap < String,
    String >,
    DemonListSchedule(RequestDemonListSchedule) => Vec < RespScheduleEntry >,
    DemonSpecDirs(RequestDemonSpecDirs) => Vec < PathBuf >,
});
