use {
    crate::{
        ipc::{
            client_req,
        },
        spec::merge_specs,
    },
    aargvark::{
        traits_impls::AargvarkJson,
        Aargvark,
    },
    flowcontrol::ta_return,
    loga::{
        ea,
        ResultContext,
    },
    puteron_lib::interface::{
        self,
        base::TaskId,
        message::{
            latest::RequestTaskOn,
            v1::{
                RequestAdd,
                RequestDemonSpecDirs,
                RequestTaskDelete,
                RequestTaskGetSpec,
                RequestTaskGetStatus,
                RequestTaskShowDownstream,
                RequestTaskShowUpstream,
                RequestTaskWaitStarted,
                RequestTaskWaitStopped,
            },
        },
    },
    tokio::{
        runtime,
    },
};

#[derive(Aargvark)]
pub(crate) struct AddArgs {
    /// ID to assign new task.
    task: TaskId,
    /// JSON task specification.
    spec: AargvarkJson<interface::task::Task>,
    /// Error if a task with the specification already exists.
    unique: Option<()>,
}

#[derive(Aargvark)]
#[vark(break_help)]
pub(crate) enum TaskCommands {
    /// Load or replace a task from a single config specified via arguments.
    Add(AddArgs),
    /// Load or replace a task with the specified id from the demon task configuration
    /// directories.
    Load(TaskId),
    /// Show the merged spec for a task from the demon task configuration directories,
    /// as it would be loaded.
    PreviewLoad(TaskId),
    /// Stop and unload a task.
    ///
    /// No error if the task is already deleted.
    Delete(TaskId),
    /// Get various runtime info about a task.
    GetStatus(TaskId),
    /// Get the merged loaded spec for a task.
    GetSpec(TaskId),
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
    WaitStarted(TaskId),
    /// Wait for a task to stop.
    ///
    /// Exits immediately if the task has already stopped. Exits with an error if the
    /// task is turned on.
    WaitStopped(TaskId),
    /// List tasks upstream of a task, plus their control and current states.
    ListUpstream(TaskId),
    /// List tasks downstream of a task, plus their control and current states.
    ListDownstream(TaskId),
}

pub(crate) fn main(command: TaskCommands) -> Result<(), loga::Error> {
    let rt = runtime::Builder::new_current_thread().enable_all().build().context("Error starting async runtime")?;
    return rt.block_on(async move {
        ta_return!((), loga::Error);
        match command {
            TaskCommands::Add(args) => {
                client_req(RequestAdd {
                    task: args.task,
                    spec: args.spec.value,
                    unique: args.unique.is_some(),
                }).await?.map_err(loga::err)?;
            },
            TaskCommands::Load(task_id) => {
                let dirs = client_req(RequestDemonSpecDirs {}).await?.map_err(loga::err)?;
                let spec =
                    merge_specs(&dirs, Some(&task_id))?
                        .remove(&task_id)
                        .context_with("Found no specs for task", ea!(task = task_id))?;
                client_req(RequestAdd {
                    task: task_id,
                    spec,
                    unique: false,
                }).await?.map_err(loga::err)?;
            },
            TaskCommands::PreviewLoad(task_id) => {
                let dirs = client_req(RequestDemonSpecDirs {}).await?.map_err(loga::err)?;
                let spec =
                    merge_specs(&dirs, Some(&task_id))?
                        .remove(&task_id)
                        .context_with("Found no specs for task", ea!(task = task_id))?;
                println!("{}", serde_json::to_string_pretty(&spec).unwrap());
            },
            TaskCommands::Delete(task_id) => {
                client_req(RequestTaskDelete(task_id)).await?.map_err(loga::err)?;
            },
            TaskCommands::GetStatus(task_id) => {
                let status = client_req(RequestTaskGetStatus(task_id)).await?.map_err(loga::err)?;
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
            },
            TaskCommands::GetSpec(task_id) => {
                let spec = client_req(RequestTaskGetSpec(task_id)).await?.map_err(loga::err)?;
                println!("{}", serde_json::to_string_pretty(&spec).unwrap());
            },
            TaskCommands::On(task_id) => {
                client_req(RequestTaskOn {
                    task: task_id,
                    on: true,
                }).await?.map_err(loga::err)?;
            },
            TaskCommands::Off(task_id) => {
                client_req(RequestTaskOn {
                    task: task_id,
                    on: false,
                }).await?.map_err(loga::err)?;
            },
            TaskCommands::WaitStarted(task_id) => {
                client_req(RequestTaskWaitStarted(task_id)).await?.map_err(loga::err)?;
            },
            TaskCommands::WaitStopped(task_id) => {
                client_req(RequestTaskWaitStopped(task_id)).await?.map_err(loga::err)?;
            },
            TaskCommands::ListUpstream(task_id) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &client_req(RequestTaskShowUpstream(task_id)).await?.map_err(loga::err)?,
                    ).unwrap()
                );
            },
            TaskCommands::ListDownstream(task_id) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &client_req(RequestTaskShowDownstream(task_id)).await?.map_err(loga::err)?,
                    ).unwrap()
                );
            },
        }
        return Ok(());
    });
}
