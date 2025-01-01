use {
    aargvark::Aargvark,
    run::DemonRunArgs,
};

mod run;
mod state;

#[derive(Aargvark)]
#[vark(break_help)]
pub(crate) enum DemonCommands {
    Run(DemonRunArgs),
}

pub(crate) fn main(command: DemonCommands) -> Result<(), loga::Error> {
    match command {
        DemonCommands::Run(args) => {
            run::main(args)?;
        },
    }
    return Ok(());
}
