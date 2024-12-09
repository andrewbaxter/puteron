use {
    aargvark::Aargvark,
    demon::DemonCommands,
    loga::fatal,
    task::TaskCommands,
};

pub mod demon;
pub mod task;
pub mod ipc;
pub mod spec;

#[derive(Aargvark)]
enum ArgCommand {
    Task(TaskCommands),
    Demon(DemonCommands),
}

#[derive(Aargvark)]
struct Args {
    command: ArgCommand,
}

fn main() {
    tracing_subscriber::fmt::init();

    fn inner() -> Result<(), loga::Error> {
        match aargvark::vark::<Args>().command {
            ArgCommand::Task(command) => task::main(command)?,
            ArgCommand::Demon(command) => demon::main(command)?,
        }
        return Ok(());
    }

    match inner() {
        Ok(_) => { },
        Err(e) => fatal(e),
    }
}
