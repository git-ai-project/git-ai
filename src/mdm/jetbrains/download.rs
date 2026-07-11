use crate::error::GitAiError;
use std::io::{Cursor, Read, Write};
use std::path::Path;

const MAX_PLUGIN_ARCHIVE_ENTRIES: usize = 4_096;
const MAX_PLUGIN_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PLUGIN_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;

fn copy_with_limit(
    reader: &mut impl Read,
    writer: &mut impl Write,
    max_bytes: u64,
) -> Result<u64, GitAiError> {
    let copied = std::io::copy(&mut reader.take(max_bytes), writer)?;
    let mut extra = [0u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(GitAiError::Generic(format!(
            "ZIP entry exceeded the {max_bytes} byte limit"
        )));
    }
    Ok(copied)
}

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

    tracing::debug!("JetBrains: Downloading plugin from {}", url);

    let agent = crate::http::build_agent(Some(120));
    let request = agent.get(&url);
    let response = crate::http::send(request)
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
    if archive.len() > MAX_PLUGIN_ARCHIVE_ENTRIES {
        return Err(GitAiError::Generic(format!(
            "Plugin ZIP exceeded the {MAX_PLUGIN_ARCHIVE_ENTRIES} entry limit ({})",
            archive.len()
        )));
    }
    let mut extracted_bytes = 0u64;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| GitAiError::Generic(format!("Failed to read ZIP entry: {}", e)))?;
        if file.size() > MAX_PLUGIN_ENTRY_BYTES
            || extracted_bytes.saturating_add(file.size()) > MAX_PLUGIN_EXTRACTED_BYTES
        {
            return Err(GitAiError::Generic(format!(
                "Plugin ZIP entry exceeded extraction limits: {} ({} bytes)",
                file.name(),
                file.size()
            )));
        }

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

            #[cfg(unix)]
            let mode = file.unix_mode();
            let remaining_bytes = MAX_PLUGIN_EXTRACTED_BYTES.saturating_sub(extracted_bytes);
            let copied = copy_with_limit(
                &mut file,
                &mut outfile,
                MAX_PLUGIN_ENTRY_BYTES.min(remaining_bytes),
            )?;
            extracted_bytes = extracted_bytes.saturating_add(copied);

            // Set executable permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = mode {
                    let permissions = std::fs::Permissions::from_mode(mode);
                    let _ = std::fs::set_permissions(&outpath, permissions);
                }
            }
        }
    }

    tracing::debug!("JetBrains: Plugin extracted to {}", plugin_dir.display());

    Ok(())
}

/// Try to install plugin using IDE CLI
///
/// Returns Ok(true) if installation succeeded, Ok(false) if CLI failed
pub fn install_plugin_via_cli(binary_path: &Path, plugin_id: &str) -> Result<bool, GitAiError> {
    use std::process::{Command, Stdio};

    tracing::debug!("JetBrains: Trying CLI installation with {:?}", binary_path);

    let result = Command::new(binary_path)
        .args(["installPlugins", plugin_id])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match result {
        Ok(status) => {
            if status.success() {
                tracing::debug!("JetBrains: CLI installation succeeded");
                Ok(true)
            } else {
                tracing::debug!("JetBrains: CLI installation failed with status {}", status);
                Ok(false)
            }
        }
        Err(e) => {
            tracing::debug!("JetBrains: Failed to run CLI: {}", e);
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::copy_with_limit;

    #[test]
    fn zip_entry_copy_rejects_data_beyond_limit_without_buffering_it() {
        let mut input = std::io::Cursor::new(vec![b'x'; 1025]);
        let mut output = Vec::new();

        let error = copy_with_limit(&mut input, &mut output, 1024)
            .expect_err("oversized entry must be rejected");
        assert!(error.to_string().contains("byte limit"));
        assert_eq!(output.len(), 1024);
    }
}
