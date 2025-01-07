use {
    crate::ipc::client_req,
    aargvark::Aargvark,
    flowcontrol::ta_return,
    loga::ResultContext,
    puteron_lib::interface::message::v1::RequestDemonEnv,
    run::DemonRunArgs,
    tokio::runtime,
};

mod run;
mod state;
mod schedule;
mod task_create_delete;
mod task_state;
mod task_util;

#[derive(Aargvark)]
#[vark(break_help)]
pub(crate) enum DemonCommands {
    /// Show the demon's effective environment variables
    Env,
    /// Run the demon in the foreground.
    Run(DemonRunArgs),
}

pub(crate) fn main(command: DemonCommands) -> Result<(), loga::Error> {
    match command {
        DemonCommands::Env => {
            let rt =
                runtime::Builder::new_current_thread().enable_all().build().context("Error starting async runtime")?;
            return rt.block_on(async move {
                ta_return!((), loga::Error);
                let status = client_req(RequestDemonEnv).await?.map_err(loga::err)?;
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
                return Ok(());
            });
        },
        DemonCommands::Run(args) => {
            run::main(args)?;
        },
    }
    return Ok(());
}
