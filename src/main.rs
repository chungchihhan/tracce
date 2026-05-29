mod cli;

use cli::Cmd;

fn main() -> std::process::ExitCode {
    let args = cli::parse();
    match args.command {
        Cmd::Trace { argv } => {
            let root = cli::root_dir();
            match peekaboo::trace::run::run(argv, root) {
                Ok(code) => std::process::ExitCode::from(code.clamp(0, 255) as u8),
                Err(e) => {
                    eprintln!("peekaboo: {e:#}");
                    std::process::ExitCode::from(2)
                }
            }
        }
        Cmd::View { .. } => {
            eprintln!("peekaboo view: not implemented yet");
            std::process::ExitCode::from(2)
        }
        Cmd::List => {
            let root = cli::root_dir();
            match peekaboo::list::run(&root) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("peekaboo: {e:#}");
                    std::process::ExitCode::from(1)
                }
            }
        }
        Cmd::FixPerms { .. } => {
            eprintln!("peekaboo fix-perms: not implemented yet");
            std::process::ExitCode::from(2)
        }
    }
}
