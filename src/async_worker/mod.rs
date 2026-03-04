pub mod dispatch;
pub mod job;
pub mod socket;
pub mod worker;

/// Handle the `git-ai async-worker` subcommand.
/// Args: --socket-path <path> --ai-dir <path>
pub fn handle_async_worker_command(args: &[String]) {
    let mut socket_path: Option<&str> = None;
    let mut ai_dir: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket-path" => {
                if i + 1 < args.len() {
                    socket_path = Some(&args[i + 1]);
                    i += 2;
                } else {
                    eprintln!("Missing value for --socket-path");
                    std::process::exit(1);
                }
            }
            "--ai-dir" => {
                if i + 1 < args.len() {
                    ai_dir = Some(&args[i + 1]);
                    i += 2;
                } else {
                    eprintln!("Missing value for --ai-dir");
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("Unknown async-worker argument: {}", args[i]);
                std::process::exit(1);
            }
        }
    }

    let socket_path = match socket_path {
        Some(p) => p,
        None => {
            eprintln!("--socket-path is required for async-worker");
            std::process::exit(1);
        }
    };

    let ai_dir = match ai_dir {
        Some(p) => p,
        None => {
            eprintln!("--ai-dir is required for async-worker");
            std::process::exit(1);
        }
    };

    worker::run_async_worker(socket_path, ai_dir);
}
