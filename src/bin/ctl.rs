use claude_architect::{Request, Response, socket_path};
use peercred_ipc::Client;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("reset") => reset(&args[1..]),
        Some("ping") => ping(),
        _ => {
            eprintln!("Usage: claude-architect-ctl <command>");
            eprintln!("  reset <project> [cwd]  Reset session and regenerate design doc");
            eprintln!("  ping                   Check if daemon is running");
            std::process::exit(1);
        }
    }
}

fn reset(args: &[String]) {
    let project = match args.first() {
        Some(p) => p.clone(),
        None => {
            eprintln!("Usage: claude-architect-ctl reset <project> [cwd]");
            std::process::exit(1);
        }
    };
    let cwd = args
        .get(1)
        .cloned()
        .or_else(|| std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_default();

    let request = Request::Reset { project, cwd };
    match Client::call::<_, Request, Response>(&socket_path(), &request) {
        Ok(Response::Verdict(msg)) => println!("{msg}"),
        Ok(Response::Error(e)) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        Ok(Response::Pong) => eprintln!("Unexpected response"),
        Err(e) => {
            eprintln!("IPC error: {e}");
            std::process::exit(1);
        }
    }
}

fn ping() {
    match Client::call::<_, Request, Response>(&socket_path(), &Request::Ping) {
        Ok(Response::Pong) => println!("ok"),
        Ok(other) => eprintln!("Unexpected: {other:?}"),
        Err(e) => {
            eprintln!("IPC error: {e}");
            std::process::exit(1);
        }
    }
}
