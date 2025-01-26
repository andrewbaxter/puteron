use {
    crate::{
        ipc_util::client_req,
        spec::merge_specs,
    },
    aargvark::{
        traits_impls::AargvarkJson,
        Aargvark,
    },
    flowcontrol::ta_return,
    loga::{
        ea,
        Log,
        ResultContext,
    },
    puteron::interface::{
        self,
        base::TaskId,
        ipc::{
            RequestTaskOnOff,
            RequestAdd,
            RequestDemonSpecDirs,
            RequestTaskDelete,
            RequestTaskGetSpec,
            RequestTaskGetStatus,
            RequestTaskListDownstream,
            RequestTaskListUpstream,
            RequestTaskListUserOn,
            RequestTaskWaitStarted,
            RequestTaskWaitStopped,
        },
    },
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
pub enum TaskCommands {
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
}

pub fn main(log: &Log, command: TaskCommands) -> Result<(), loga::Error> {
    let rt = runtime::Builder::new_current_thread().enable_all().build().context("Error starting async runtime")?;
    return rt.block_on(async move {
        ta_return!((), loga::Error);
        match command {
            TaskCommands::Load(args) => {
                client_req(RequestAdd {
                    task: args.task,
                    spec: args.spec.value,
                    unique: args.unique.is_some(),
                }).await?;
            },
            TaskCommands::LoadStored(task_id) => {
                let dirs = client_req(RequestDemonSpecDirs {}).await?;
                let spec =
                    merge_specs(log, &dirs, Some(&task_id))?
                        .remove(&task_id)
                        .context_with("Found no specs for task", ea!(task = task_id))?;
                client_req(RequestAdd {
                    task: task_id,
                    spec,
                    unique: false,
                }).await?;
            },
            TaskCommands::PreviewStored(task_id) => {
                let dirs = client_req(RequestDemonSpecDirs {}).await?;
                let spec =
                    merge_specs(log, &dirs, Some(&task_id))?
                        .remove(&task_id)
                        .context_with("Found no specs for task", ea!(task = task_id))?;
                println!("{}", serde_json::to_string_pretty(&spec).unwrap());
            },
            TaskCommands::Delete(task_id) => {
                client_req(RequestTaskDelete(task_id)).await?;
            },
            TaskCommands::Status(task_id) => {
                let status = client_req(RequestTaskGetStatus(task_id)).await?;
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
            },
            TaskCommands::Spec(task_id) => {
                let spec = client_req(RequestTaskGetSpec(task_id)).await?;
                println!("{}", serde_json::to_string_pretty(&spec).unwrap());
            },
            TaskCommands::On(task_id) => {
                client_req(RequestTaskOnOff {
                    task: task_id,
                    on: true,
                }).await?;
            },
            TaskCommands::Off(task_id) => {
                client_req(RequestTaskOnOff {
                    task: task_id,
                    on: false,
                }).await?;
            },
            TaskCommands::WaitUntilStarted(task_id) => {
                client_req(RequestTaskWaitStarted(task_id)).await?;
            },
            TaskCommands::WaitUntilStopped(task_id) => {
                client_req(RequestTaskWaitStopped(task_id)).await?;
            },
            TaskCommands::ListUserOn => {
                println!("{}", serde_json::to_string_pretty(&client_req(RequestTaskListUserOn).await?).unwrap());
            },
            TaskCommands::ListUpstream(task_id) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&client_req(RequestTaskListUpstream(task_id)).await?).unwrap()
                );
            },
            TaskCommands::ListDownstream(task_id) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&client_req(RequestTaskListDownstream(task_id)).await?).unwrap()
                );
            },
        }
        return Ok(());
    });
}
