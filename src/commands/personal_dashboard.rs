use crate::config;

/// Handle the `git-ai personal-dashboard` command
pub fn handle_personal_dashboard(_args: &[String]) {
    let config = config::Config::get();
    let api_base_url = config.api_base_url();

    let dashboard_url = format!("{}/me", api_base_url);

    eprintln!("Opening dashboard: {}", dashboard_url);

    if open_browser(&dashboard_url).is_err() {
        eprintln!("Could not open browser automatically.");
        eprintln!("Visit this URL in your browser:");
        eprintln!("  {}", dashboard_url);
    }
}

/// Attempt to open a URL in the system's default browser
fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}
