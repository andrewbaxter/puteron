use {
    crate::interface::{
        base::TaskId,
        task::Task,
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
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
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

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
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

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct RequestTaskDelete(pub TaskId);

impl Into<Request> for RequestTaskDelete {
    fn into(self) -> Request {
        return Request::TaskDelete(self);
    }
}

impl RequestTrait for RequestTaskDelete {
    type Response = Result<(), String>;
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct RequestTaskGetStatus(pub TaskId);

impl Into<Request> for RequestTaskGetStatus {
    fn into(self) -> Request {
        return Request::TaskGetStatus(self);
    }
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct TaskStatusSpecificEmpty {
    pub started: bool,
    pub started_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub enum ProcState {
    Stopped,
    Starting,
    Started,
    Stopping,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct TaskStatusSpecificPerpetual {
    pub state: ProcState,
    pub state_at: DateTime<Utc>,
    pub pid: Option<i32>,
    pub restarts: usize,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct TaskStatusSpecificFinite {
    pub state: ProcState,
    pub state_at: DateTime<Utc>,
    pub pid: Option<i32>,
    pub restarts: usize,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub enum TaskStatusSpecific {
    Empty(TaskStatusSpecificEmpty),
    Perpetual(TaskStatusSpecificPerpetual),
    Finite(TaskStatusSpecificFinite),
    External,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
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

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct RequestTaskGetSpec(pub TaskId);

impl Into<Request> for RequestTaskGetSpec {
    fn into(self) -> Request {
        return Request::TaskGetSpec(self);
    }
}

impl RequestTrait for RequestTaskGetSpec {
    type Response = Result<Task, String>;
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct RequestTaskWaitStarted(pub TaskId);

impl Into<Request> for RequestTaskWaitStarted {
    fn into(self) -> Request {
        return Request::TaskWaitStarted(self);
    }
}

impl RequestTrait for RequestTaskWaitStarted {
    type Response = Result<(), String>;
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct RequestTaskWaitStopped(pub TaskId);

impl Into<Request> for RequestTaskWaitStopped {
    fn into(self) -> Request {
        return Request::TaskWaitStopped(self);
    }
}

impl RequestTrait for RequestTaskWaitStopped {
    type Response = Result<(), String>;
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct TaskUpstreamStatus {
    pub task: TaskId,
    pub on: bool,
    pub started: bool,
    pub strong: bool,
    pub related: HashMap<TaskId, TaskUpstreamStatus>,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct RequestTaskShowUpstream(pub TaskId);

impl Into<Request> for RequestTaskShowUpstream {
    fn into(self) -> Request {
        return Request::TaskShowUpstream(self);
    }
}

impl RequestTrait for RequestTaskShowUpstream {
    type Response = Result<HashMap<TaskId, TaskUpstreamStatus>, String>;
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct TaskDownstreamStatus {
    pub task: TaskId,
    pub on: bool,
    pub started: bool,
    pub related: HashMap<TaskId, TaskDownstreamStatus>,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
pub struct RequestTaskShowDownstream(pub TaskId);

impl Into<Request> for RequestTaskShowDownstream {
    fn into(self) -> Request {
        return Request::TaskShowDownstream(self);
    }
}

impl RequestTrait for RequestTaskShowDownstream {
    type Response = Result<HashMap<TaskId, TaskDownstreamStatus>, String>;
}

// # Demon
#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename = "snake_case", deny_unknown_fields)]
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
#[serde(rename = "snake_case", deny_unknown_fields)]
pub enum Request {
    TaskAdd(RequestAdd),
    TaskDelete(RequestTaskDelete),
    TaskGetStatus(RequestTaskGetStatus),
    TaskGetSpec(RequestTaskGetSpec),
    TaskOn(RequestTaskOn),
    TaskWaitStarted(RequestTaskWaitStarted),
    TaskWaitStopped(RequestTaskWaitStopped),
    TaskShowUpstream(RequestTaskShowUpstream),
    TaskShowDownstream(RequestTaskShowDownstream),
    DemonSpecDirs(RequestDemonSpecDirs),
}
