use {
    aargvark::{
        traits_impls::AargvarkJson,
        Aargvark,
    },
    flowcontrol::ta_return,
    loga::{
        ea,
        fatal,
        Log,
        ResultContext,
    },
    puteron::interface::{
        self,
        base::TaskId,
        ipc::{
            ProcState,
            RequestDemonEnv,
            RequestDemonSpecDirs,
            RequestTaskAdd,
            RequestTaskDelete,
            RequestTaskGetSpec,
            RequestTaskGetStatus,
            RequestTaskList,
            RequestTaskListDownstream,
            RequestTaskListUpstream,
            RequestTaskListUserOn,
            RequestTaskOnOff,
            RequestTaskWaitStarted,
            RequestTaskWaitStopped,
            TaskStatusSpecific,
        },
    },
    puteron_bin::{
        demon::{
            self,
            DemonRunArgs,
        },
        ipc_util::{
            client,
            client_req,
        },
        spec::merge_specs,
    },
    serde::Serialize,
    std::collections::HashMap,
    tokio::runtime,
};

#[derive(Aargvark)]
pub struct LoadArgs {
    /// ID to assign new task.
    task: TaskId,
    /// JSON task specification.
    spec: AargvarkJson<interface::task::Task>,
    /// Error if a task with the specification already exists.
    unique: Option<()>,
}

#[derive(Aargvark)]
#[vark(break_help)]
enum ArgCommand {
    Overview,
    /// Load or replace a task from a single config specified via arguments.
    Load(LoadArgs),
    /// Load or replace a task with the specified id from the demon task configuration
    /// directories.
    LoadStored(TaskId),
    /// Show the merged spec for a task from the demon task configuration directories,
    /// as it would be loaded.
    PreviewStored(TaskId),
    /// Stop and unload a task.
    ///
    /// No error if the task is already deleted.
    Delete(TaskId),
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
    /// List tasks upstream of a task, plus their control and current states.
    ListUpstream(TaskId),
    /// List tasks downstream of a task, plus their control and current states.
    ListDownstream(TaskId),
    /// Show the demon's effective environment variables
    Env,
    /// List the current schedule. This includes the next time of all scheduled tasks.
    /// The schedule is in ascending scheduled activation time.
    ListSchedule,
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
    let log = Log::new_root(match args.debug.is_some() {
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
                    state: String,
                }

                let mut out = HashMap::new();
                for task in tasks {
                    let status = client.send_req(RequestTaskGetStatus(task.clone())).await.map_err(loga::err)?;
                    const STATE_STARTING: &str = "starting";
                    const STATE_STARTED: &str = "started";
                    const STATE_STOPPING: &str = "stopping";
                    const STATE_STOPPED: &str = "stopped";
                    out.insert(task, Entry {
                        on: status.direct_on || status.transitive_on,
                        state: match status.specific {
                            TaskStatusSpecific::Empty(specific) => match specific.started {
                                true => STATE_STARTED.to_string(),
                                false => STATE_STOPPED.to_string(),
                            },
                            TaskStatusSpecific::Long(specific) => match specific.state {
                                ProcState::Stopped => STATE_STOPPED.to_string(),
                                ProcState::Starting => STATE_STARTING.to_string(),
                                ProcState::Started => STATE_STARTED.to_string(),
                                ProcState::Stopping => STATE_STOPPING.to_string(),
                            },
                            TaskStatusSpecific::Short(specific) => match specific.state {
                                ProcState::Stopped => STATE_STOPPED.to_string(),
                                ProcState::Starting => STATE_STARTING.to_string(),
                                ProcState::Started => STATE_STARTED.to_string(),
                                ProcState::Stopping => STATE_STOPPING.to_string(),
                            },
                        },
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
            ArgCommand::LoadStored(task_id) => {
                let dirs = client_req(RequestDemonSpecDirs {}).await?;
                let spec =
                    merge_specs(&log, &dirs, Some(&task_id))
                        .await?
                        .remove(&task_id)
                        .context_with("Found no specs for task", ea!(task = task_id))?;
                client_req(RequestTaskAdd {
                    task: task_id,
                    spec,
                    unique: false,
                }).await?;
            },
            ArgCommand::PreviewStored(task_id) => {
                let dirs = client_req(RequestDemonSpecDirs {}).await?;
                let spec =
                    merge_specs(&log, &dirs, Some(&task_id))
                        .await?
                        .remove(&task_id)
                        .context_with("Found no specs for task", ea!(task = task_id))?;
                println!("{}", serde_json::to_string_pretty(&spec).unwrap());
            },
            ArgCommand::Delete(task_id) => {
                client_req(RequestTaskDelete(task_id)).await?;
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
            ArgCommand::Env => {
                let rt =
                    runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .context("Error starting async runtime")?;
                return rt.block_on(async move {
                    ta_return!((), loga::Error);
                    let status = client_req(RequestDemonEnv).await?;
                    println!("{}", serde_json::to_string_pretty(&status).unwrap());
                    return Ok(());
                });
            },
            ArgCommand::ListSchedule => {
                let rt =
                    runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .context("Error starting async runtime")?;
                return rt.block_on(async move {
                    ta_return!((), loga::Error);
                    let status = client_req(RequestDemonEnv).await?;
                    println!("{}", serde_json::to_string_pretty(&status).unwrap());
                    return Ok(());
                });
            },
            ArgCommand::Demon(args) => {
                demon::main(&log, args).await?;
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
