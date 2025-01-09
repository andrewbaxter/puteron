use {
    aargvark::Aargvark,
    loga::{
        fatal,
        Log,
    },
    puteron::{
        demon,
        task,
    },
};

#[derive(Aargvark)]
#[vark(break_help)]
enum ArgCommand {
    Task(task::TaskCommands),
    Demon(demon::DemonCommands),
}

#[derive(Aargvark)]
struct Args {
    command: ArgCommand,
    /// Log at debug level
    debug: Option<()>,
}

fn main() {
    let args = aargvark::vark::<Args>();
    let log = Log::new_root(match args.debug.is_some() {
        true => loga::DEBUG,
        false => loga::INFO,
    });
    match (|| {
        match args.command {
            ArgCommand::Task(command) => task::main(&log, command)?,
            ArgCommand::Demon(command) => demon::main(&log, command)?,
        }
        return Ok(());
    })() {
        Ok(_) => { },
        Err(e) => {
            fatal(e);
        },
    }
}
