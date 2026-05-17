use git_ai::auth::state::{AuthState, collect_auth_status, format_unix_timestamp};

pub fn handle_whoami(args: &[String]) {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        eprintln!("git-ai whoami - Show current auth state and identity");
        eprintln!();
        eprintln!("Usage:");
        eprintln!("  git-ai whoami");
        return;
    }

    let auth = collect_auth_status();

    println!("Credential backend: {}", auth.backend);

    match &auth.state {
        AuthState::LoggedOut => {
            println!("Auth state: logged out");
            std::process::exit(1);
        }
        AuthState::LoggedIn => {
            println!("Auth state: logged in");
        }
        AuthState::RefreshExpired => {
            println!("Auth state: credentials expired (refresh token expired)");
            std::process::exit(1);
        }
        AuthState::Error(err) => {
            println!("Auth state: error ({})", err);
            std::process::exit(1);
        }
    }

    if let Some(expires_at) = auth.access_token_expires_at {
        println!(
            "Access token expires at: {}",
            format_unix_timestamp(expires_at)
        );
    }
    if let Some(expires_at) = auth.refresh_token_expires_at {
        println!(
            "Refresh token expires at: {}",
            format_unix_timestamp(expires_at)
        );
    }

    println!(
        "User ID: {}",
        auth.user_id.unwrap_or_else(|| "<unavailable>".to_string())
    );
    println!(
        "Email: {}",
        auth.email.unwrap_or_else(|| "<unavailable>".to_string())
    );
    println!(
        "Name: {}",
        auth.name.unwrap_or_else(|| "<unavailable>".to_string())
    );
    println!(
        "Personal org ID: {}",
        auth.personal_org_id
            .unwrap_or_else(|| "<unavailable>".to_string())
    );

    if auth.orgs.is_empty() {
        println!("Organizations: <none>");
    } else {
        println!("Organizations:");
        for org in auth.orgs {
            let org_id = org.org_id.unwrap_or_else(|| "<unknown-id>".to_string());
            let org_slug = org.org_slug.unwrap_or_else(|| "<unknown-slug>".to_string());
            let org_name = org.org_name.unwrap_or_else(|| "<unknown-name>".to_string());
            let role = org.role.unwrap_or_else(|| "<unknown-role>".to_string());
            println!("  - {} ({}) [{}] role={}", org_slug, org_name, org_id, role);
        }
    }
}
