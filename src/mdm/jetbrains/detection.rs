use super::ide_types::{DetectedIde, JETBRAINS_IDES, JetBrainsIde};
use crate::mdm::utils::home_dir;
#[cfg(target_os = "macos")]
use crate::utils::debug_log;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

/// Find all installed JetBrains IDEs on the system
pub fn find_jetbrains_installations() -> Vec<DetectedIde> {
    let mut detected = Vec::new();

    #[cfg(target_os = "macos")]
    {
        detected.extend(find_macos_installations());
    }

    #[cfg(windows)]
    {
        detected.extend(find_windows_installations());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        detected.extend(find_linux_installations());
    }

    detected
}

// ===== macOS Detection =====

#[cfg(target_os = "macos")]
fn find_macos_installations() -> Vec<DetectedIde> {
    let mut detected = Vec::new();

    for ide in JETBRAINS_IDES {
        for bundle_id in ide.bundle_ids {
            if let Some(app_path) = find_app_by_bundle_id(bundle_id)
                && let Some(detected_ide) = detect_macos_ide(ide, &app_path)
            {
                detected.push(detected_ide);
            }
        }
    }

    // Also scan common installation directories
    let scan_dirs = vec![
        PathBuf::from("/Applications"),
        home_dir().join("Applications"),
        home_dir().join("Applications/JetBrains Toolbox"),
    ];

    for scan_dir in scan_dirs {
        if scan_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&scan_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "app") {
                    for ide in JETBRAINS_IDES {
                        if is_matching_macos_app(ide, &path)
                            && let Some(detected_ide) = detect_macos_ide(ide, &path)
                        {
                            // Avoid duplicates
                            if !detected
                                .iter()
                                .any(|d| d.install_path == detected_ide.install_path)
                            {
                                detected.push(detected_ide);
                            }
                        }
                    }
                }
            }
        }
    }

    detected
}

#[cfg(target_os = "macos")]
fn find_app_by_bundle_id(bundle_id: &str) -> Option<PathBuf> {
    let output = Command::new("mdfind")
        .args([&format!("kMDItemCFBundleIdentifier == '{}'", bundle_id)])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().next().map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn is_matching_macos_app(ide: &JetBrainsIde, app_path: &Path) -> bool {
    let app_name = app_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    let app_name_lower = app_name.to_lowercase();

    // Match based on IDE name patterns
    match ide.product_code {
        "IU" | "IC" => app_name_lower.contains("intellij"),
        "PY" | "PC" => app_name_lower.contains("pycharm"),
        "WS" => app_name_lower.contains("webstorm"),
        "GO" => app_name_lower.contains("goland"),
        "CL" => app_name_lower.contains("clion"),
        "PS" => app_name_lower.contains("phpstorm"),
        "RD" => app_name_lower.contains("rider"),
        "RM" => app_name_lower.contains("rubymine"),
        "DB" => app_name_lower.contains("datagrip"),
        "AI" => app_name_lower.contains("android studio"),
        _ => false,
    }
}

#[cfg(target_os = "macos")]
fn detect_macos_ide(ide: &'static JetBrainsIde, app_path: &Path) -> Option<DetectedIde> {
    let binary_path = app_path
        .join("Contents")
        .join("MacOS")
        .join(ide.binary_name_macos);

    if !binary_path.exists() {
        debug_log(&format!(
            "JetBrains: Binary not found at {:?} for {}",
            binary_path, ide.name
        ));
        return None;
    }

    // Get build number from Info.plist or product-info.json
    let (build_number, major_build) = get_macos_build_number(app_path);

    // Get plugins directory
    let plugins_dir = get_plugins_dir(ide.product_code, build_number.as_deref());

    Some(DetectedIde {
        ide,
        install_path: app_path.to_path_buf(),
        binary_path,
        build_number,
        major_build,
        plugins_dir,
    })
}

#[cfg(target_os = "macos")]
fn get_macos_build_number(app_path: &Path) -> (Option<String>, Option<u32>) {
    // Try product-info.json first (newer JetBrains IDEs)
    let product_info_path = app_path.join("Contents/Resources/product-info.json");
    if product_info_path.exists()
        && let Ok(content) = std::fs::read_to_string(&product_info_path)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(build) = json.get("buildNumber").and_then(|v| v.as_str())
    {
        let major = parse_major_build(build);
        return (Some(build.to_string()), major);
    }

    // Fall back to Info.plist
    let output = Command::new("defaults")
        .args([
            "read",
            &app_path.join("Contents/Info.plist").to_string_lossy(),
            "CFBundleVersion",
        ])
        .output()
        .ok();

    if let Some(output) = output
        && output.status.success()
    {
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let major = parse_major_build(&version);
        return (Some(version), major);
    }

    (None, None)
}

// ===== Windows Detection =====

#[cfg(windows)]
fn find_windows_installations() -> Vec<DetectedIde> {
    let mut detected = Vec::new();

    // Scan Toolbox directory
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let toolbox_apps = PathBuf::from(&local_app_data)
            .join("JetBrains")
            .join("Toolbox")
            .join("apps");

        if toolbox_apps.exists() {
            detected.extend(scan_windows_toolbox_dir(&toolbox_apps));
        }
    }

    // Scan Program Files directories
    let program_dirs = vec![
        std::env::var("ProgramFiles").ok().map(PathBuf::from),
        std::env::var("ProgramFiles(x86)").ok().map(PathBuf::from),
    ];

    for program_dir in program_dirs.into_iter().flatten() {
        let jetbrains_dir = program_dir.join("JetBrains");
        if jetbrains_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&jetbrains_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    for ide in JETBRAINS_IDES {
                        if let Some(detected_ide) = detect_windows_ide(ide, &path)
                            && !detected
                                .iter()
                                .any(|d| d.install_path == detected_ide.install_path)
                        {
                            detected.push(detected_ide);
                        }
                    }
                }
            }
        }
    }

    detected
}

#[cfg(windows)]
fn scan_windows_toolbox_dir(toolbox_apps: &Path) -> Vec<DetectedIde> {
    let mut detected = Vec::new();

    if let Ok(entries) = std::fs::read_dir(toolbox_apps) {
        for entry in entries.flatten() {
            let app_dir = entry.path();
            if !app_dir.is_dir() {
                continue;
            }

            // Find matching IDE by toolbox app name
            let dir_name = app_dir.file_name().and_then(|s| s.to_str()).unwrap_or("");

            for ide in JETBRAINS_IDES {
                if dir_name.contains(ide.toolbox_app_name)
                    && let Ok(versions) = std::fs::read_dir(&app_dir)
                {
                    for version_entry in versions.flatten() {
                        let version_dir = version_entry.path();
                        if version_dir.is_dir()
                            && let Some(detected_ide) = detect_windows_ide(ide, &version_dir)
                        {
                            detected.push(detected_ide);
                        }
                    }
                }
            }
        }
    }

    detected
}

#[cfg(windows)]
fn detect_windows_ide(ide: &'static JetBrainsIde, install_path: &Path) -> Option<DetectedIde> {
    let binary_path = install_path.join("bin").join(ide.binary_name_windows);

    if !binary_path.exists() {
        return None;
    }

    let (build_number, major_build) = get_windows_build_number(install_path);
    let plugins_dir = get_plugins_dir(ide.product_code, build_number.as_deref());

    Some(DetectedIde {
        ide,
        install_path: install_path.to_path_buf(),
        binary_path,
        build_number,
        major_build,
        plugins_dir,
    })
}

#[cfg(windows)]
fn get_windows_build_number(install_path: &Path) -> (Option<String>, Option<u32>) {
    let product_info_path = install_path.join("product-info.json");
    if product_info_path.exists()
        && let Ok(content) = std::fs::read_to_string(&product_info_path)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(build) = json.get("buildNumber").and_then(|v| v.as_str())
    {
        let major = parse_major_build(build);
        return (Some(build.to_string()), major);
    }

    (None, None)
}

// ===== Linux Detection =====

#[cfg(all(unix, not(target_os = "macos")))]
fn find_linux_installations() -> Vec<DetectedIde> {
    let mut detected = Vec::new();

    // Scan Toolbox directory
    let toolbox_apps = home_dir()
        .join(".local")
        .join("share")
        .join("JetBrains")
        .join("Toolbox")
        .join("apps");

    if toolbox_apps.exists() {
        detected.extend(scan_linux_toolbox_dir(&toolbox_apps));
    }

    // Scan common installation directories
    let scan_dirs = vec![
        home_dir().join(".local").join("share").join("JetBrains"),
        PathBuf::from("/opt"),
        PathBuf::from("/usr/local"),
    ];

    for scan_dir in scan_dirs {
        if scan_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&scan_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    for ide in JETBRAINS_IDES {
                        if let Some(detected_ide) = detect_linux_ide(ide, &path)
                            && !detected
                                .iter()
                                .any(|d| d.install_path == detected_ide.install_path)
                        {
                            detected.push(detected_ide);
                        }
                    }
                }
            }
        }
    }

    detected
}

#[cfg(all(unix, not(target_os = "macos")))]
fn scan_linux_toolbox_dir(toolbox_apps: &Path) -> Vec<DetectedIde> {
    let mut detected = Vec::new();

    if let Ok(entries) = std::fs::read_dir(toolbox_apps) {
        for entry in entries.flatten() {
            let app_dir = entry.path();
            if !app_dir.is_dir() {
                continue;
            }

            let dir_name = app_dir.file_name().and_then(|s| s.to_str()).unwrap_or("");

            for ide in JETBRAINS_IDES {
                if dir_name.contains(ide.toolbox_app_name) {
                    // Toolbox uses versioned subdirectories with a "ch-0" pattern
                    if let Ok(channels) = std::fs::read_dir(&app_dir) {
                        for channel_entry in channels.flatten() {
                            let channel_dir = channel_entry.path();
                            if channel_dir.is_dir() {
                                // Inside channel, there are version directories
                                if let Ok(versions) = std::fs::read_dir(&channel_dir) {
                                    for version_entry in versions.flatten() {
                                        let version_dir = version_entry.path();
                                        if version_dir.is_dir()
                                            && let Some(detected_ide) =
                                                detect_linux_ide(ide, &version_dir)
                                        {
                                            detected.push(detected_ide);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    detected
}

#[cfg(all(unix, not(target_os = "macos")))]
fn detect_linux_ide(ide: &'static JetBrainsIde, install_path: &Path) -> Option<DetectedIde> {
    let binary_path = install_path.join("bin").join(ide.binary_name_linux);

    if !binary_path.exists() {
        return None;
    }

    let (build_number, major_build) = get_linux_build_number(install_path);
    let plugins_dir = get_plugins_dir(ide.product_code, build_number.as_deref());

    Some(DetectedIde {
        ide,
        install_path: install_path.to_path_buf(),
        binary_path,
        build_number,
        major_build,
        plugins_dir,
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn get_linux_build_number(install_path: &Path) -> (Option<String>, Option<u32>) {
    let product_info_path = install_path.join("product-info.json");
    if product_info_path.exists()
        && let Ok(content) = std::fs::read_to_string(&product_info_path)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(build) = json.get("buildNumber").and_then(|v| v.as_str())
    {
        let major = parse_major_build(build);
        return (Some(build.to_string()), major);
    }

    (None, None)
}

// ===== Shared Utilities =====

/// Parse the major build number from a build string like "252.12345.67"
fn parse_major_build(build: &str) -> Option<u32> {
    build.split('.').next()?.parse().ok()
}

/// Get the plugins directory for an IDE
fn get_plugins_dir(product_code: &str, build_number: Option<&str>) -> PathBuf {
    // Extract the version year from build number (e.g., "252" -> "2025.2")
    let version_suffix = build_number
        .and_then(parse_major_build)
        .map(|major| {
            // Build 252 = 2025.2, 251 = 2025.1, 243 = 2024.3, etc.
            let year = 2000 + (major / 10);
            let minor = major % 10;
            format!("{}{}.{}", product_code, year, minor)
        })
        .unwrap_or_else(|| product_code.to_string());

    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("JetBrains")
            .join(&version_suffix)
            .join("plugins")
    }

    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            PathBuf::from(appdata)
                .join("JetBrains")
                .join(&version_suffix)
                .join("plugins")
        } else {
            home_dir()
                .join("AppData")
                .join("Roaming")
                .join("JetBrains")
                .join(&version_suffix)
                .join("plugins")
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        home_dir()
            .join(".local")
            .join("share")
            .join("JetBrains")
            .join(&version_suffix)
            .join("plugins")
    }
}

/// Check if the Git AI plugin is installed for a detected IDE
pub fn is_plugin_installed(detected: &DetectedIde) -> bool {
    let plugin_dir = detected.plugins_dir.join("git-ai-intellij");
    plugin_dir.exists()
}
