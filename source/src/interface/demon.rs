use {
    schemars::JsonSchema,
    serde::{
        Deserialize,
        Serialize,
    },
    std::{
        collections::{
            HashMap,
        },
        path::PathBuf,
    },
};

#[derive(Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Environment {
    /// Inherit all parent environment variables. If this is true, `keep` is ignored.
    #[serde(default)]
    pub keep_all: bool,
    /// A map of environment variables and a bool, whether inherit from the context's
    /// parent environment variable pool. The bool is required for allowing overrides
    /// when merging configs, normally all entries would be `true`.
    #[serde(default)]
    pub keep: HashMap<String, bool>,
    /// Add or override the following environment variables;
    #[serde(default)]
    pub add: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LogType {
    /// Default, all child stdout/stderr output goes to the puteron's stderr.
    #[default]
    Stderr,
    /// All child stdout/stderr is sent to the syslog using the task id as the syslog
    /// name.
    Syslog,
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    #[serde(rename = "$schema", skip_serializing)]
    pub _schema: Option<String>,
    #[serde(default)]
    pub environment: Environment,
    #[serde(default)]
    pub task_dirs: Vec<PathBuf>,
    /// Synchronize tasks to the contents of the task directories.
    ///
    /// This means that when a task file is deleted, the task will be deleted. When a
    /// new task file is found, a task will be created. This may miss events in WSL:
    /// https://github.com/notify-rs/notify/issues/254 . This may miss events in large
    /// directories: https://github.com/notify-rs/notify/issues/412 .
    #[serde(default)]
    pub watch: bool,
    #[serde(default)]
    pub log_type: LogType,
}
