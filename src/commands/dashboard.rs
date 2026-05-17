const DEFAULT_DASHBOARD_URL: &str = "https://gitai.co";

pub fn handle_dashboard(_args: &[String]) {
    let base_url =
        std::env::var("GIT_AI_API_URL").unwrap_or_else(|_| DEFAULT_DASHBOARD_URL.to_string());
    let dashboard_url = format!("{}/me", base_url.trim_end_matches('/'));

    eprintln!("Opening dashboard: {}", dashboard_url);

    if open_browser(&dashboard_url).is_err() {
        eprintln!("Could not open browser automatically.");
        eprintln!("Visit this URL in your browser:");
        eprintln!("  {}", dashboard_url);
    }
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}
