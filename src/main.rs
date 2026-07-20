mod cli;

use cli::Cmd;
use tracce::trace::provider::Provider;

fn main() -> std::process::ExitCode {
    let args = cli::parse();
    let root = cli::root_dir();

    match args.command {
        None => print_hint(),
        Some(Cmd::Claude { args }) => trace_agent(Provider::Claude, args, root),
        Some(Cmd::Codex { args }) => trace_agent(Provider::Codex, args, root),
        Some(Cmd::Exec { argv }) => trace_cmd(argv, Provider::Other, root),

        Some(Cmd::Attach { agent, pid }) => match tracce::trace::attach::run(agent, pid, &root) {
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

fn print_hint() -> std::process::ExitCode {
    println!("tracce — trace coding-agent activity on macOS\n");
    println!("  tracce claude [args…]    trace Claude Code CLI");
    println!("  tracce codex [args…]     trace Codex CLI");
    println!("  tracce attach [pid]      watch a running agent in the live board");
    println!("  tracce view              replay a recorded session");
    println!("  tracce --help            show all options");
    std::process::ExitCode::SUCCESS
}

/// Launch an agent with the given forwarded arguments.
fn trace_agent(
    provider: Provider,
    args: Vec<String>,
    root: std::path::PathBuf,
) -> std::process::ExitCode {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(provider.command().expect("agent providers have commands").to_string());
    argv.extend(args);
    trace_cmd(argv, provider, root)
}

fn trace_cmd(
    argv: Vec<String>,
    provider: Provider,
    root: std::path::PathBuf,
) -> std::process::ExitCode {
    match tracce::trace::run::run(argv, provider, root) {
        Ok(code) => std::process::ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("tracce: {e:#}");
            std::process::ExitCode::from(2)
        }
    }
}
