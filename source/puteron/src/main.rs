use {
    aargvark::Aargvark,
    demon::DemonCommands,
    task::TaskCommands,
    tracing::{
        level_filters::LevelFilter,
    },
};

pub mod demon;
pub mod task;
pub mod ipc;
pub mod spec;

#[derive(Aargvark)]
#[vark(break_help)]
enum ArgCommand {
    Task(TaskCommands),
    Demon(DemonCommands),
}

#[derive(Aargvark)]
struct Args {
    command: ArgCommand,
    /// Log at debug level
    debug: Option<()>,
}

fn main() -> Result<(), loga::Error> {
    let args = aargvark::vark::<Args>();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt::Subscriber::builder().with_max_level(if args.debug.is_some() {
            LevelFilter::DEBUG
        } else {
            LevelFilter::INFO
        }).finish(),
    ).unwrap();
    match args.command {
        ArgCommand::Task(command) => task::main(command)?,
        ArgCommand::Demon(command) => demon::main(command)?,
    }
    return Ok(());
}
