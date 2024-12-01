use {
    aargvark::{
        traits_impls::AargvarkJson,
        Aargvark,
    },
    loga::{
        ea,
        fatal,
        ErrContext,
        Log,
        ResultContext,
    },
    serde::{
        Deserialize,
        Serialize,
    },
    std::{
        collections::{
            hash_map::Entry,
            HashMap,
        },
        env,
        fs::{
            read,
            read_dir,
        },
        io::ErrorKind,
        os::unix::net::UnixStream,
        path::PathBuf,
    },
    structre::structre,
    tokio::runtime,
};

#[derive(Clone, Copy)]
enum SimpleDurationUnit {
    Second,
    Minute,
    Hour,
}

#[derive(Clone, Copy)]
struct SimpleDuration {
    count: usize,
    unit: SimpleDurationUnit,
}

const SIMPLE_DURATION_UNIT_SECOND: &str = "s";
const SIMPLE_DURATION_UNIT_MINUTE: &str = "m";
const SIMPLE_DURATION_UNIT_HOUR: &str = "h";

impl Serialize for SimpleDuration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer {
        return format_args!("{}{}", self.count, match self.unit {
            SimpleDurationUnit::Second => SIMPLE_DURATION_UNIT_SECOND,
            SimpleDurationUnit::Minute => SIMPLE_DURATION_UNIT_MINUTE,
            SimpleDurationUnit::Hour => SIMPLE_DURATION_UNIT_HOUR,
        }).serialize(serializer);
    }
}

impl<'de> Deserialize<'de> for SimpleDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de> {
        #[structre("(?<count>\\d+)(?<unit>[a-z]+)")]
        struct Parser<'a> {
            count: usize,
            unit: &'a str,
        }

        let s = <&str>::deserialize(deserializer)?;
        let p = Parser::try_from(s).map_err(|e| serde::de::Error::custom(e))?;
        return Ok(Self {
            count: p.count,
            unit: match p.unit {
                SIMPLE_DURATION_UNIT_SECOND => SimpleDurationUnit::Second,
                SIMPLE_DURATION_UNIT_MINUTE => SimpleDurationUnit::Minute,
                SIMPLE_DURATION_UNIT_HOUR => SimpleDurationUnit::Hour,
                s => return Err(serde::de::Error::custom(format!("Unknown time unit suffix [{}]", s))),
            },
        });
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
enum FiniteTaskEndAction {
    /// Nothing happens, task continues to be considered on and started.
    None,
    /// Set the user-on state to `false` once the task ends.
    TurnOff,
    /// Delete the task once the task ends. It will no longer show up in output and
    /// will be considered off.
    Delete,
}

impl Default for FiniteTaskEndAction {
    fn default() -> Self {
        return Self::None;
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
struct FiniteTaskConfig {
    /// Kill the one-shot task if it doesn't complete in this amount of time.
    #[serde(default)]
    timeout: Option<SimpleDuration>,
    #[serde(default)]
    end_action: FiniteTaskEndAction,
    /// Which exit codes are considered success.  By default, `0`.
    #[serde(default)]
    success_codes: Vec<u16>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
enum TaskType {
    /// A task that continues to run until stopped.
    ///
    /// Perpetual tasks are considered started immediately, unless a `start_check`
    /// command is provided.
    Perpetual,
    /// A task that stops on its own.
    ///
    /// Finite tasks are considered started once they successfully exit.
    Finite(FiniteTaskConfig),
}

impl Default for TaskType {
    fn default() -> Self {
        return Self::Perpetual;
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
struct EnvironmentConfig {
    /// If present, a map of environment variables and a bool, whether inherit from the
    /// context's parent environment variable pool. The bool is required for allowing
    /// overrides when merging configs, normally all entries would be `true`.
    clear: Option<HashMap<String, bool>>,
    /// Add or override the following environment variables;
    add: HashMap<String, String>,
}

impl Default for EnvironmentConfig {
    fn default() -> Self {
        return Self {
            clear: Some([].into_iter().collect()),
            add: Default::default(),
        };
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
struct RestartConfig {
    /// How long to wait before restarting the task. Defaults to 1 minute.
    delay: Option<SimpleDuration>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
enum DependencyType {
    /// Does the following:
    ///
    /// * Sets `transitive_on` to true on the dependency
    ///
    /// * The dependency must have started before starting this task
    ///
    /// * Stops this task if the dependency stops
    Hard,
    /// Does the following
    ///
    /// * The dependency must have started before starting this task
    ///
    /// * Stops this task if the dependency stops
    Soft,
    /// Does the following:
    ///
    /// * If the dependency is on, this task won't start before the dependency starts
    ///
    /// * If the dependency is not on, this task may start immediately
    ///
    /// * Stops this task if the dependency stops but is still on
    Optional,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
struct TaskDefinition {
    #[serde(default)]
    r#type: Option<TaskType>,
    #[serde(default)]
    default_off: bool,
    #[serde(default)]
    working_directory: Option<PathBuf>,
    #[serde(default)]
    environment: EnvironmentConfig,
    command: Vec<String>,
    /// Restart the command if it fails or unexpectedly exits (perpetual tasks only).
    #[serde(default)]
    restart: Option<RestartConfig>,
    #[serde(default)]
    dependencies: HashMap<String, DependencyType>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "snake_case", deny_unknown_fields)]
struct Config {
    #[serde(default)]
    environment: EnvironmentConfig,
    #[serde(default)]
    task_dirs: Vec<PathBuf>,
}

#[derive(Aargvark)]
struct CreateArgs {
    name: Option<String>,
    task: AargvarkJson<TaskDefinition>,
}

#[derive(Aargvark)]
struct EnsureArgs {
    name: String,
    task: AargvarkJson<TaskDefinition>,
}

#[derive(Aargvark)]
enum ControlCommands {
    /// Retrieve the current status of the demon.
    DemonInfo,
    /// Retrieve a list of tasks.
    List,
    /// Set `user_on` for the specified task to `true`.
    On(String),
    /// Set `user_on` for the specified task to `false`.
    Off(String),
    /// Retrieve the current status and definition of the task.
    TaskInfo(String),
    /// Retrieve a list of tasks this task is waiting on to start.
    WaitingFor(String),
    /// Retrieve a list of tasks waiting on this task to start.
    ReverseWaitingFor(String),
    /// Create a task with an automatically-generated unique id
    Create(CreateArgs),
    /// Create a task with the specified id if no task with that id already exists.
    Ensure(EnsureArgs),
    /// Delete a task. The task will be considered off and not started. Be careful,
    /// tasks may be difficult to restore once deleted.
    Delete(String),
}

#[derive(Aargvark)]
struct DemonArgs {
    config: AargvarkJson<Config>,
}

#[derive(Aargvark)]
enum ArgCommand {
    Control(ControlCommands),
    Launch(DemonArgs),
}

#[derive(Aargvark)]
struct Args {
    command: ArgCommand,
}

fn main1() -> Result<(), loga::Error> {
    match aargvark::vark::<Args>().command {
        ArgCommand::Control(command) => {
            let conn = UnixStream::connect(daemon_path());
            match command {
                ControlCommands::DemonInfo => todo!(),
                ControlCommands::List => todo!(),
                ControlCommands::On(_) => todo!(),
                ControlCommands::Off(_) => todo!(),
                ControlCommands::TaskInfo(_) => todo!(),
                ControlCommands::WaitingFor(_) => todo!(),
                ControlCommands::ReverseWaitingFor(_) => todo!(),
                ControlCommands::Create(_) => todo!(),
                ControlCommands::Ensure(_) => todo!(),
                ControlCommands::Delete(_) => todo!(),
            }
        },
        ArgCommand::Launch(args) => {
            let log = Log::new_root(loga::INFO);
            let config = args.config.value;

            // Combine configs
            let mut task_json = HashMap::new();
            for dir in config.task_dirs {
                let dir_entries = match read_dir(&dir) {
                    Ok(e) => e,
                    Err(e) => {
                        if e.kind() == ErrorKind::NotFound {
                            log.log_with(
                                loga::DEBUG,
                                "Task directory doesn't exist, skipping",
                                ea!(dir = dir.to_string_lossy()),
                            );
                            continue;
                        }
                        return Err(
                            e.context_with(
                                "Unable to read specified task directory",
                                ea!(dir = dir.to_string_lossy()),
                            ),
                        );
                    },
                };
                for e in dir_entries {
                    let e = e.context("Error reading task directory entry")?;
                    let path = e.path();
                    let mut config =
                        serde_json::from_slice::<serde_json::Value>(
                            &read(
                                &path,
                            ).context_with(
                                "Error reading json from task directory",
                                ea!(path = path.to_string_lossy()),
                            )?,
                        ).context_with("Task definition has invalid json", ea!(path = path.to_string_lossy()))?;
                    let task_name =
                        String::from_utf8(
                            path.file_stem().unwrap().as_encoded_bytes().to_vec(),
                        ).context_with(
                            "Task directory entry has invalid unicode name",
                            ea!(path = path.to_string_lossy()),
                        )?;
                    if let Some(lower) = task_json.remove(&task_name) {
                        fn merge(lower: serde_json::Value, upper: serde_json::Value) -> serde_json::Value {
                            match (lower, upper) {
                                (serde_json::Value::Object(mut lower), serde_json::Value::Object(upper)) => {
                                    for (k, mut upper_child) in upper {
                                        if let Some(lower_child) = lower.remove(&k) {
                                            upper_child = merge(lower_child, upper_child);
                                        }
                                        lower.insert(k, upper_child);
                                    }
                                    return serde_json::Value::Object(lower);
                                },
                                (_, upper) => {
                                    return upper;
                                },
                            }
                        }

                        config = merge(lower, config);
                    }
                    task_json.insert(task_name, config);
                }
            }

            // Prep env
            let env = HashMap::new();
            match config.environment.clear {
                Some(keep) => {
                    for (k, ok) in keep {
                        if !ok {
                            continue;
                        }
                        if let Ok(v) = env::var(&k) {
                            env.insert(k, v);
                        }
                    }
                },
                None => {
                    env.extend(env::vars());
                },
            }
            env.extend(config.environment.add);

            // Start initial on tasks
            let mut tasks = HashMap::new();

            struct TaskState {
                user_on: bool,
                transitive_on: bool,
                config: TaskDefinition,
            }

            for (id, value) in task_json {
                let config =
                    serde_json::from_value::<TaskDefinition>(
                        value,
                    ).context_with(
                        "Task has invalid definition",
                        ea!(id = id, config = serde_json::to_string_pretty(&value).unwrap()),
                    )?;
                tasks.insert(id, TaskState {
                    user_on: !config.default_off,
                    transitive_on: false,
                    config: config,
                });
            }
            let rt =
                runtime::Builder::new_current_thread().enable_all().build().context("Error starting async runtime")?;
            for (id, task) in &tasks {
                if task.user_on {
                    start_task(id, task);
                }
            }
            rt.block_on(async {
                shutdown_signal.await;
            });

            // Handle commands
            tm.spawn();

            // Run until signalled
            tm.join(&log).await;
        },
    }
    return Ok(());
}

fn main() {
    match main1() {
        Ok(_) => { },
        Err(e) => fatal(e),
    }
}
