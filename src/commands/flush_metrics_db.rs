use git_ai::daemon::telemetry_worker;

pub fn handle_flush_metrics_db(args: &[String]) {
    let show_stats = args.first().map(|a| a == "--stats" || a == "-s").unwrap_or(false);

    if show_stats {
        match telemetry_worker::queue_stats() {
            Ok((metrics, cas)) => {
                println!("Offline telemetry queue:");
                println!("  pending metrics batches: {}", metrics);
                println!("  pending CAS batches:     {}", cas);
            }
            Err(e) => {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    eprintln!("Flushing offline telemetry queue...");
    match telemetry_worker::flush_queue_now() {
        Ok((metrics, cas)) => {
            if metrics == 0 && cas == 0 {
                println!("Queue is empty, nothing to flush.");
            } else {
                println!(
                    "Flushed {} metric event(s) and {} CAS object(s).",
                    metrics, cas
                );
            }
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
