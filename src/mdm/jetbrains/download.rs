use crate::error::GitAiError;
use crate::utils::debug_log;
use std::io::{Cursor, Read};
use std::path::Path;

/// Download plugin from JetBrains Marketplace
///
/// Returns the ZIP file contents as bytes
pub fn download_plugin_from_marketplace(
    plugin_id: &str,
    product_code: &str,
    build_number: &str,
) -> Result<Vec<u8>, GitAiError> {
    let url = format!(
        "https://plugins.jetbrains.com/pluginManager?action=download&id={}&build={}-{}",
        plugin_id, product_code, build_number
    );

    debug_log(&format!("JetBrains: Downloading plugin from {}", url));

    let response = minreq::get(&url)
        .with_timeout(120) // 120 second timeout for plugin download
        .send()
        .map_err(|e| GitAiError::Generic(format!("Failed to download plugin: {}", e)))?;

    if response.status_code == 404 {
        return Err(GitAiError::Generic(
            "Plugin not found in JetBrains Marketplace. It may not be published yet.".to_string(),
        ));
    }

    if response.status_code != 200 {
        return Err(GitAiError::Generic(format!(
            "JetBrains Marketplace returned status {}",
            response.status_code
        )));
    }

    Ok(response.into_bytes())
}

/// Extract plugin ZIP to plugins directory
///
/// The ZIP file should contain a directory structure that will be extracted
/// directly into the plugins directory
pub fn install_plugin_to_directory(zip_data: &[u8], plugin_dir: &Path) -> Result<(), GitAiError> {
    use zip::ZipArchive;

    // Ensure the plugins directory exists
    if !plugin_dir.exists() {
        std::fs::create_dir_all(plugin_dir).map_err(|e| {
            GitAiError::Generic(format!(
                "Failed to create plugins directory {}: {}",
                plugin_dir.display(),
                e
            ))
        })?;
    }

    let cursor = Cursor::new(zip_data);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|e| GitAiError::Generic(format!("Failed to read plugin ZIP: {}", e)))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| GitAiError::Generic(format!("Failed to read ZIP entry: {}", e)))?;

        let outpath = match file.enclosed_name() {
            Some(path) => plugin_dir.join(path),
            None => continue,
        };

        if file.name().ends_with('/') {
            // Directory entry
            std::fs::create_dir_all(&outpath).map_err(|e| {
                GitAiError::Generic(format!(
                    "Failed to create directory {}: {}",
                    outpath.display(),
                    e
                ))
            })?;
        } else {
            // File entry
            if let Some(parent) = outpath.parent()
                && !parent.exists()
            {
                std::fs::create_dir_all(parent).map_err(|e| {
                    GitAiError::Generic(format!(
                        "Failed to create parent directory {}: {}",
                        parent.display(),
                        e
                    ))
                })?;
            }

            let mut outfile = std::fs::File::create(&outpath).map_err(|e| {
                GitAiError::Generic(format!(
                    "Failed to create file {}: {}",
                    outpath.display(),
                    e
                ))
            })?;

            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer).map_err(|e| {
                GitAiError::Generic(format!("Failed to read ZIP file contents: {}", e))
            })?;

            std::io::Write::write_all(&mut outfile, &buffer).map_err(|e| {
                GitAiError::Generic(format!("Failed to write file {}: {}", outpath.display(), e))
            })?;

            // Set executable permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    let permissions = std::fs::Permissions::from_mode(mode);
                    let _ = std::fs::set_permissions(&outpath, permissions);
                }
            }
        }
    }

    debug_log(&format!(
        "JetBrains: Plugin extracted to {}",
        plugin_dir.display()
    ));

    Ok(())
}

/// Try to install plugin using IDE CLI
///
/// Returns Ok(true) if installation succeeded, Ok(false) if CLI failed
pub fn install_plugin_via_cli(binary_path: &Path, plugin_id: &str) -> Result<bool, GitAiError> {
    use std::process::Command;

    debug_log(&format!(
        "JetBrains: Trying CLI installation with {:?}",
        binary_path
    ));

    #[cfg(windows)]
    let result = Command::new(binary_path)
        .args(["installPlugins", plugin_id])
        .output();

    #[cfg(not(windows))]
    let result = Command::new(binary_path)
        .args(["installPlugins", plugin_id])
        .output();

    match result {
        Ok(output) => {
            if output.status.success() {
                debug_log("JetBrains: CLI installation succeeded");
                Ok(true)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug_log(&format!("JetBrains: CLI installation failed: {}", stderr));
                Ok(false)
            }
        }
        Err(e) => {
            debug_log(&format!("JetBrains: Failed to run CLI: {}", e));
            Ok(false)
        }
    }
}
