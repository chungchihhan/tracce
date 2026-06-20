mod cli;

use cli::Cmd;

fn main() -> std::process::ExitCode {
    let args = cli::parse();
    let root = cli::root_dir();

    match args.command {
        // Bare `tracce` and `tracce claude …` both launch+record claude.
        None => trace_claude(Vec::new(), root),
        Some(Cmd::Claude { args }) => trace_claude(args, root),
        Some(Cmd::Exec { argv }) => trace_cmd(argv, root),

        Some(Cmd::Attach { pid }) => match tracce::trace::attach::run(pid, &root) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("tracce: {e:#}");
                std::process::ExitCode::from(1)
            }
        },

        Some(Cmd::View { target, latest, no_follow }) => {
            match tracce::view::run::run(target, latest, no_follow, &root) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("tracce: {e:#}");
                    std::process::ExitCode::from(1)
                }
            }
        }

        Some(Cmd::List) => match tracce::list::run(&root) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("tracce: {e:#}");
                std::process::ExitCode::from(1)
            }
        },

        Some(Cmd::FixPerms { session }) => match tracce::fix_perms::run(&root, session) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("tracce: {e:#}");
                std::process::ExitCode::from(1)
            }
        },
    }
}

/// `tracce` / `tracce claude …` → trace `claude` with the given forwarded args.
fn trace_claude(args: Vec<String>, root: std::path::PathBuf) -> std::process::ExitCode {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push("claude".to_string());
    argv.extend(args);
    trace_cmd(argv, root)
}

fn trace_cmd(argv: Vec<String>, root: std::path::PathBuf) -> std::process::ExitCode {
    match tracce::trace::run::run(argv, root) {
        Ok(code) => std::process::ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("tracce: {e:#}");
            std::process::ExitCode::from(2)
        }
    }
}
