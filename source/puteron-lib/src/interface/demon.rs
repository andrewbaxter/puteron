use {
    schemars::JsonSchema,
    serde::{
        Deserialize,
        Serialize,
    },
    std::{
        collections::HashMap,
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

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub environment: Environment,
    #[serde(default)]
    pub task_dirs: Vec<PathBuf>,
}
