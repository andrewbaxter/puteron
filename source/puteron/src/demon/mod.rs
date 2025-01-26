use {
    crate::ipc::client_req,
    aargvark::Aargvark,
    flowcontrol::ta_return,
    loga::{
        Log,
        ResultContext,
    },
    puteron_lib::interface::message::RequestDemonEnv,
    run::DemonRunArgs,
    tokio::runtime,
};

mod run;
mod state;
mod schedule;
mod task_create_delete;
mod task_util;
mod task_execute;
mod task_plan;
mod task_plan_test;

#[derive(Aargvark)]
#[vark(break_help)]
pub enum DemonCommands {
    /// Show the demon's effective environment variables
    Env,
    /// List the current schedule. This includes the next time of all scheduled tasks.
    /// The schedule is in ascending scheduled activation time.
    ListSchedule,
    /// Run the demon in the foreground.
    Run(DemonRunArgs),
}

pub fn main(log: &Log, command: DemonCommands) -> Result<(), loga::Error> {
    match command {
        DemonCommands::Env => {
            let rt =
                runtime::Builder::new_current_thread().enable_all().build().context("Error starting async runtime")?;
            return rt.block_on(async move {
                ta_return!((), loga::Error);
                let status = client_req(RequestDemonEnv).await?;
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
                return Ok(());
            });
        },
        DemonCommands::ListSchedule => {
            let rt =
                runtime::Builder::new_current_thread().enable_all().build().context("Error starting async runtime")?;
            return rt.block_on(async move {
                ta_return!((), loga::Error);
                let status = client_req(RequestDemonEnv).await?;
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
                return Ok(());
            });
        },
        DemonCommands::Run(args) => {
            run::main(log, args)?;
        },
    }
    return Ok(());
}
