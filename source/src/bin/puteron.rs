use {
    aargvark::{
        Aargvark,
        traits_impls::AargvarkJson,
    },
    loga::{
        Log,
        ResultContext,
        ea,
        fatal,
    },
    puteron::{
        demon::{
            self,
            DemonRunArgs,
        },
        errors::{
            ErrorHandler,
            ReturnErrorHandler,
        },
        interface::{
            base::TaskId,
            demon::Config,
            ipc::{
                Actual,
                ReqTaskWatch,
                RequestDemonEnv,
                RequestDemonSpecDirs,
                RequestTaskAdd,
                RequestTaskDelete,
                RequestTaskGetSpec,
                RequestTaskGetStatus,
                RequestTaskList,
                RequestTaskListBlockingStart,
                RequestTaskListBlockingStop,
                RequestTaskListDownstream,
                RequestTaskListUpstream,
                RequestTaskListUserOn,
                RequestTaskOnOff,
                RequestTaskWaitStarted,
                RequestTaskWaitStopped,
            },
            task::{
                ShortTaskStartedAction,
                Task,
            },
        },
        ipc_util::{
            client,
            client_req,
        },
        spec::{
            list_task_dir_tasks,
            merge_specs,
            order_specs,
        },
    },
    serde::Serialize,
    std::{
        collections::{
            BTreeMap,
            HashMap,
        },
        path::PathBuf,
    },
    tokio::io::AsyncWriteExt,
};

#[derive(Aargvark)]
pub struct LoadArgs {
    /// ID to assign new task.
    task: TaskId,
    /// JSON task specification.
    spec: AargvarkJson<Task>,
    /// Error if a task with the specification already exists.
    unique: Option<()>,
}

#[derive(Aargvark)]
pub struct DeleteArgs {
    /// ID of task to delete.
    task: TaskId,
    /// Mark all downstream dependencies for deletion.
    recurse: Option<()>,
    /// Set the task to off (as well as any dependencies, if recursive).
    off: Option<()>,
    /// Wait for task to be deleted before exiting.
    wait: Option<()>,
}

#[derive(Aargvark)]
#[vark(break_help)]
enum ArgCommand {
    Overview,
    /// Load or replace a task from a single config specified via arguments.
    Load(LoadArgs),
    /// Show the merged spec for a task from the demon task configuration directories,
    /// as it would be loaded.
    PreviewStored(TaskId),
    /// Get various runtime info about a task.
    Status(TaskId),
    /// Get the merged loaded spec for a task.
    Spec(TaskId),
    /// Turn a task on.
    ///
    /// No error if the task is already on.
    On(TaskId),
    /// Turn a task off.
    ///
    /// No error if the task is already off.
    Off(TaskId),
    /// Mark a task to be deleted when stopped.
    ///
    /// This action is uncancellable, but you can re-add the task afterwards. This
    /// requires all downstream tasks to also be marked for deletion. No error if the
    /// task is already marked for deletion.
    Delete(DeleteArgs),
    /// Wait for a task to start.
    ///
    /// Exits immediately if the task has already started. Exits with an error if the
    /// task is turned off.
    WaitUntilStarted(TaskId),
    /// Wait for a task to stop.
    ///
    /// Exits immediately if the task has already stopped. Exits with an error if the
    /// task is turned on.
    WaitUntilStopped(TaskId),
    /// List tasks that are user-on.
    ListUserOn,
    /// List leaf tasks (upstream) that are blocking the current task from starting.
    ListBlockingStart(TaskId),
    /// List leaf tasks (downstream) that are blocking the current task from stopping.
    ListBlockingStop(TaskId),
    /// List tasks upstream of a task, plus their control and current states.
    ListUpstream(TaskId),
    /// List tasks downstream of a task, plus their control and current states.
    ListDownstream(TaskId),
    /// Writes a line of JSON to stdout every time a task's control or actual state
    /// changes.
    Watch,
    /// Show the demon's effective environment variables
    Env,
    /// List the current schedule. This includes the next time of all scheduled tasks.
    /// The schedule is in ascending scheduled activation time.
    ListSchedule,
    /// Validate that the config can be parsed and is valid per early checks.
    ValidateConfig {
        _unused: AargvarkJson<Config>,
    },
    /// Validate that the task configs can be parsed and are valid per early checks (no
    /// dependency cycles, etc) and exit, don't run anything. Takes the list of task
    /// directories to merve and validate.
    ValidateTaskConfigs(Vec<PathBuf>),
    /// Run the demon in the foreground.
    Demon(DemonRunArgs),
}

#[derive(Aargvark)]
struct Args {
    command: ArgCommand,
    /// Log at debug level
    debug: Option<()>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args = aargvark::vark::<Args>();
    let debug = args.debug.is_some();
    let log = Log::new_root(match debug {
        true => loga::DEBUG,
        false => loga::INFO,
    });
    match async {
        match args.command {
            ArgCommand::Overview => {
                let mut client = client().await?;
                let tasks = client.send_req(RequestTaskList).await.map_err(loga::err)?;

                #[derive(Serialize)]
                struct Entry {
                    on: bool,
                    actual: Actual,
                }

                let mut out = BTreeMap::new();
                for task in tasks {
                    let status = client.send_req(RequestTaskGetStatus(task.clone())).await.map_err(loga::err)?;
                    out.insert(task, Entry {
                        on: status.effective_on,
                        actual: status.actual,
                    });
                }
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
            },
            ArgCommand::Load(args) => {
                client_req(RequestTaskAdd {
                    task: args.task,
                    spec: args.spec.value,
                    unique: args.unique.is_some(),
                }).await?;
            },
            ArgCommand::PreviewStored(task_id) => {
                let dirs = client_req(RequestDemonSpecDirs {}).await?;
                let mut errors = ReturnErrorHandler { errors: Default::default() };
                let mut fs_tasks = list_task_dir_tasks(&dirs, &mut errors).await;
                fs_tasks.retain(|k, _v| *k == task_id);
                let spec =
                    merge_specs(fs_tasks, &mut errors)
                        .await
                        .remove(&task_id)
                        .context_with("Found no specs for task", ea!(task = task_id))?;
                if !errors.errors.is_empty() {
                    return Err(loga::agg_err("Errors occurred while assembling task data", errors.errors));
                }
                println!("{}", serde_json::to_string_pretty(&spec).unwrap());
            },
            ArgCommand::Delete(args) => {
                client_req(RequestTaskDelete {
                    task: args.task,
                    recurse: args.recurse.is_some(),
                    off: args.off.is_some(),
                    wait: args.wait.is_some(),
                }).await?;
            },
            ArgCommand::Status(task_id) => {
                let status = client_req(RequestTaskGetStatus(task_id)).await?;
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
            },
            ArgCommand::Spec(task_id) => {
                let spec = client_req(RequestTaskGetSpec(task_id)).await?;
                println!("{}", serde_json::to_string_pretty(&spec).unwrap());
            },
            ArgCommand::On(task_id) => {
                client_req(RequestTaskOnOff {
                    task: task_id,
                    on: true,
                }).await?;
            },
            ArgCommand::Off(task_id) => {
                client_req(RequestTaskOnOff {
                    task: task_id,
                    on: false,
                }).await?;
            },
            ArgCommand::WaitUntilStarted(task_id) => {
                client_req(RequestTaskWaitStarted(task_id)).await?;
            },
            ArgCommand::WaitUntilStopped(task_id) => {
                client_req(RequestTaskWaitStopped(task_id)).await?;
            },
            ArgCommand::ListUserOn => {
                println!("{}", serde_json::to_string_pretty(&client_req(RequestTaskListUserOn).await?).unwrap());
            },
            ArgCommand::ListBlockingStart(task_id) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&client_req(RequestTaskListBlockingStart(task_id)).await?).unwrap()
                );
            },
            ArgCommand::ListBlockingStop(task_id) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&client_req(RequestTaskListBlockingStop(task_id)).await?).unwrap()
                );
            },
            ArgCommand::ListUpstream(task_id) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&client_req(RequestTaskListUpstream(task_id)).await?).unwrap()
                );
            },
            ArgCommand::ListDownstream(task_id) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&client_req(RequestTaskListDownstream(task_id)).await?).unwrap()
                );
            },
            ArgCommand::Watch => {
                let mut stdout = tokio::io::stdout();
                loop {
                    let events = client_req(ReqTaskWatch).await?;
                    for event in events {
                        stdout.write_all(format!("{}\n", serde_json::to_string(&event).unwrap()).as_bytes()).await?;
                    }
                    stdout.flush().await?;
                }
            },
            ArgCommand::Env => {
                let status = client_req(RequestDemonEnv).await?;
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
            },
            ArgCommand::ListSchedule => {
                let status = client_req(RequestDemonEnv).await?;
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
            },
            ArgCommand::ValidateConfig { .. } => {
                // nop, already checked by aargvark
            },
            ArgCommand::ValidateTaskConfigs(dirs) => {
                let mut errors = ReturnErrorHandler { errors: Default::default() };
                let fs_tasks = list_task_dir_tasks(&dirs, &mut errors).await;
                let new_specs = merge_specs(fs_tasks, &mut errors).await;
                let new_specs =
                    order_specs(Default::default(), &mut errors, new_specs.clone())
                        .0
                        .into_iter()
                        .collect::<HashMap<_, _>>();
                for (k, v) in &new_specs {
                    for (upstream_k, _) in match v {
                        Task::Empty(s) => &s.upstream,
                        Task::Long(s) => &s.upstream,
                        Task::Short(s) => &s.upstream,
                    } {
                        match new_specs.get(upstream_k).unwrap() {
                            Task::Empty(_us) => { },
                            Task::Long(_us) => { },
                            Task::Short(us) => {
                                match us.started_action {
                                    Some(ShortTaskStartedAction::TurnOff) => {
                                        errors.handle(
                                            loga::err(
                                                &format!(
                                                    "Task [{}] has upstream [{}] with started action turn-off so [{}] will never be able to start",
                                                    k,
                                                    upstream_k,
                                                    k
                                                ),
                                            ),
                                        );
                                    },
                                    Some(ShortTaskStartedAction::None) => { },
                                    None => { },
                                }
                            },
                        }
                    }
                }

                // TODO do additional validation... cycle detection, missing upstreams, bad
                // upstream parameters (started_action off). These currently exist but depend on
                // the state so it'd have to be abstracted somehow.
                if !errors.errors.is_empty() {
                    return Err(
                        loga::agg_err("One or more errors occurred while checking task configs", errors.errors),
                    );
                }
            },
            ArgCommand::Demon(args) => {
                demon::main(debug, &log, args).await?;
            },
        }
        return Ok(());
    }.await {
        Ok(_) => { },
        Err(e) => {
            fatal(e);
        },
    }
}
