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

        Some(Cmd::Export { target, output, latest }) => {
            match export_session(target, output, latest, &root) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("tracce: {e:#}");
                    std::process::ExitCode::from(1)
                }
            }
        }

        Some(Cmd::Import { file, force }) => match tracce::bundle::import(&file, &root, force) {
            Ok(id) => {
                println!("imported session {id}");
                println!("replay with: tracce view {id}");
                std::process::ExitCode::SUCCESS
            }
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

/// Resolve a session and export it; default output is ./<id>.tracce.tgz.
fn export_session(
    target: Option<String>,
    output: Option<std::path::PathBuf>,
    latest: bool,
    root: &std::path::Path,
) -> anyhow::Result<()> {
    let entry = tracce::view::run::resolve_entry(target, latest, root)?;
    let out = output
        .unwrap_or_else(|| std::path::PathBuf::from(format!("{}.tracce.tgz", entry.meta.session_id)));
    let n = tracce::bundle::export(&entry, &out)?;
    println!("exported {} ({} bytes) -> {}", entry.meta.session_id, n, out.display());
    Ok(())
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
