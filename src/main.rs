mod cli;

use cli::Cmd;

fn main() -> std::process::ExitCode {
    let args = cli::parse();
    match args.command {
        Cmd::Trace { argv: _ } => {
            eprintln!("peekaboo trace: not implemented yet");
            std::process::ExitCode::from(2)
        }
        Cmd::View { .. } => {
            eprintln!("peekaboo view: not implemented yet");
            std::process::ExitCode::from(2)
        }
        Cmd::List => {
            // empty table is success
            println!("STATUS\tSTARTED\tCWD\tCLAUDE_PID\tEVENTS\tSESSION_ID");
            std::process::ExitCode::SUCCESS
        }
        Cmd::FixPerms { .. } => {
            eprintln!("peekaboo fix-perms: not implemented yet");
            std::process::ExitCode::from(2)
        }
    }
}
