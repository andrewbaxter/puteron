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
    puteron::{
        demon::{
            self,
            DemonRunArgs,
        },
        interface::{
            self,
            base::TaskId,
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
                RequestTaskListDownstream,
                RequestTaskListUpstream,
                RequestTaskListUserOn,
                RequestTaskOnOff,
                RequestTaskWaitStarted,
                RequestTaskWaitStopped,
            },
        },
        ipc_util::{
            client,
            client_req,
        },
        spec::merge_specs,
    },
    serde::Serialize,
    std::collections::HashMap,
    tokio::{
        io::AsyncWriteExt,
        runtime,
    },
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

/// List upstream dependencies of a task. By default, this only shows strong,
/// non-started dependencies (those that prevent the target task from starting).
#[derive(Aargvark)]
pub struct ListUpstreamArgs {
    /// List upstreams of this task
    task: TaskId,
    include_started: Option<()>,
    /// Shorthand for all `include-` options.
    #[vark(flag = "--all", flag = "-a")]
    all: Option<()>,
}

/// List downstream dependencies of a task. By default, this only shows strong,
/// non-stopped dependencies (those that prevent the target task from stopping).
#[derive(Aargvark)]
pub struct ListDownstreamArgs {
    /// List downstreams of this task
    task: TaskId,
    include_weak: Option<()>,
    include_stopped: Option<()>,
    /// Shorthand for all `include-` options.
    #[vark(flag = "--all", flag = "-a")]
    all: Option<()>,
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
    ListUpstream(ListUpstreamArgs),
    /// List tasks downstream of a task, plus their control and current states.
    ListDownstream(ListDownstreamArgs),
    /// Writes a line of JSON to stdout every time a task's control or actual state
    /// changes.
    Watch,
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

                let mut out = HashMap::new();
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
            ArgCommand::ListUpstream(args) => {
                println!("{}", serde_json::to_string_pretty(&client_req(RequestTaskListUpstream {
                    task: args.task,
                    include_started: args.include_started.is_some() || args.all.is_some(),
                }).await?).unwrap());
            },
            ArgCommand::ListDownstream(args) => {
                println!("{}", serde_json::to_string_pretty(&client_req(RequestTaskListDownstream {
                    task: args.task,
                    include_stopped: args.include_stopped.is_some() || args.all.is_some(),
                    include_weak: args.include_weak.is_some() || args.all.is_some(),
                }).await?).unwrap());
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
