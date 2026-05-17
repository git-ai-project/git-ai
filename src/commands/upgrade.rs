use std::process::Command;

pub fn handle_upgrade(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("git-ai upgrade - Update git-ai to the latest version");
        println!();
        println!("Usage: git-ai upgrade [--check]");
        println!();
        println!("Options:");
        println!("  --check   Check for updates without installing");
        return;
    }

    let check_only = args.iter().any(|a| a == "--check");

    println!("git-ai v{}", env!("CARGO_PKG_VERSION"));
    println!();

    if check_only {
        println!("Update check not yet implemented in v2.");
        println!("Reinstall via: curl -fsSL https://gitai.co/install | sh");
        return;
    }

    println!("Upgrading git-ai...");
    println!();

    let result = Command::new("sh")
        .args(["-c", "curl -fsSL https://gitai.co/install | sh"])
        .status();

    match result {
        Ok(status) if status.success() => {
            println!();
            println!("Upgrade complete.");
        }
        Ok(status) => {
            eprintln!("Upgrade failed (exit code {})", status.code().unwrap_or(1));
            eprintln!("Try manually: curl -fsSL https://gitai.co/install | sh");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to run upgrade: {}", e);
            eprintln!("Try manually: curl -fsSL https://gitai.co/install | sh");
            std::process::exit(1);
        }
    }
}
