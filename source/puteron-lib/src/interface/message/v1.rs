use {
    crate::interface::{
        base::TaskId,
        task::{
            DependencyType,
            Task,
        },
    },
    chrono::{
        DateTime,
        Utc,
    },
    schemars::JsonSchema,
    serde::{
        de::DeserializeOwned,
        Deserialize,
        Serialize,
    },
    std::{
        collections::HashMap,
        path::PathBuf,
    },
};

pub trait RequestTrait: Serialize + DeserializeOwned + Into<Request> {
    type Response: Serialize + DeserializeOwned;
}

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

impl Into<Request> for RequestAdd {
    fn into(self) -> Request {
        return Request::TaskAdd(self);
    }
}

impl RequestTrait for RequestAdd {
    type Response = Result<(), String>;
}

// On/off
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskOn {
    pub task: TaskId,
    pub on: bool,
}

impl Into<Request> for RequestTaskOn {
    fn into(self) -> Request {
        return Request::TaskOn(self);
    }
}

impl RequestTrait for RequestTaskOn {
    type Response = Result<(), String>;
}

// Delete
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskDelete(pub TaskId);

impl Into<Request> for RequestTaskDelete {
    fn into(self) -> Request {
        return Request::TaskDelete(self);
    }
}

impl RequestTrait for RequestTaskDelete {
    type Response = Result<(), String>;
}

// Status
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskGetStatus(pub TaskId);

impl Into<Request> for RequestTaskGetStatus {
    fn into(self) -> Request {
        return Request::TaskGetStatus(self);
    }
}

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

impl RequestTrait for RequestTaskGetStatus {
    type Response = Result<TaskStatus, String>;
}

// Get spec
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskGetSpec(pub TaskId);

impl Into<Request> for RequestTaskGetSpec {
    fn into(self) -> Request {
        return Request::TaskGetSpec(self);
    }
}

impl RequestTrait for RequestTaskGetSpec {
    type Response = Result<Task, String>;
}

// Wait started
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskWaitStarted(pub TaskId);

impl Into<Request> for RequestTaskWaitStarted {
    fn into(self) -> Request {
        return Request::TaskWaitStarted(self);
    }
}

impl RequestTrait for RequestTaskWaitStarted {
    type Response = Result<(), String>;
}

// Wait stopped
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskWaitStopped(pub TaskId);

impl Into<Request> for RequestTaskWaitStopped {
    fn into(self) -> Request {
        return Request::TaskWaitStopped(self);
    }
}

impl RequestTrait for RequestTaskWaitStopped {
    type Response = Result<(), String>;
}

// List user-on
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskListUserOn;

impl Into<Request> for RequestTaskListUserOn {
    fn into(self) -> Request {
        return Request::TaskListUserOn(self);
    }
}

impl RequestTrait for RequestTaskListUserOn {
    type Response = Result<Vec<TaskId>, String>;
}

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

impl Into<Request> for RequestTaskListUpstream {
    fn into(self) -> Request {
        return Request::TaskListUpstream(self);
    }
}

impl RequestTrait for RequestTaskListUpstream {
    type Response = Result<HashMap<TaskId, TaskDependencyStatus>, String>;
}

// List downstream
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestTaskListDownstream(pub TaskId);

impl Into<Request> for RequestTaskListDownstream {
    fn into(self) -> Request {
        return Request::TaskListDownstream(self);
    }
}

impl RequestTrait for RequestTaskListDownstream {
    type Response = Result<HashMap<TaskId, TaskDependencyStatus>, String>;
}

// # Demon
//
// Effective environment
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestDemonEnv;

impl Into<Request> for RequestDemonEnv {
    fn into(self) -> Request {
        return Request::DemonEnv(self);
    }
}

impl RequestTrait for RequestDemonEnv {
    type Response = Result<HashMap<String, String>, String>;
}

// Spec dirs
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestDemonSpecDirs {}

impl Into<Request> for RequestDemonSpecDirs {
    fn into(self) -> Request {
        return Request::DemonSpecDirs(self);
    }
}

impl RequestTrait for RequestDemonSpecDirs {
    type Response = Result<Vec<PathBuf>, String>;
}

// # All together
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    TaskAdd(RequestAdd),
    TaskDelete(RequestTaskDelete),
    TaskGetStatus(RequestTaskGetStatus),
    TaskGetSpec(RequestTaskGetSpec),
    TaskOn(RequestTaskOn),
    TaskWaitStarted(RequestTaskWaitStarted),
    TaskWaitStopped(RequestTaskWaitStopped),
    TaskListUserOn(RequestTaskListUserOn),
    TaskListUpstream(RequestTaskListUpstream),
    TaskListDownstream(RequestTaskListDownstream),
    DemonEnv(RequestDemonEnv),
    DemonSpecDirs(RequestDemonSpecDirs),
}
