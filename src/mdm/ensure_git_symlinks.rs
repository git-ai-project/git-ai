use crate::error::GitAiError;
use crate::git::repository::exec_git;
use std::path::PathBuf;

/// Ensures the libexec symlink exists for Fork compatibility.
/// Creates a symlink from <binary_parent>/../libexec to the real git's libexec.
pub fn ensure_git_symlinks() -> Result<(), GitAiError> {
    // Get current executable path
    let exe_path = std::env::current_exe()?;

    // Skip symlink creation if running from Nix store (read-only filesystem)
    // or other read-only install locations. In these cases, the packaging system
    // (e.g., Nix flake) should handle creating the libexec symlink at build time.
    if exe_path.to_string_lossy().contains("/nix/store") {
        return Ok(());
    }

    // Get parent directories: binary_dir is e.g. ~/.git-ai/bin, base_dir is ~/.git-ai
    let binary_dir = exe_path
        .parent()
        .ok_or_else(|| GitAiError::Generic("Cannot get binary directory".to_string()))?;
    let base_dir = binary_dir
        .parent()
        .ok_or_else(|| GitAiError::Generic("Cannot get base directory".to_string()))?;

    // Get real git's exec-path (e.g. /usr/libexec/git-core)
    let output = exec_git(&["--exec-path".to_string()])?;
    let exec_path = String::from_utf8(output.stdout)?.trim().to_string();
    let exec_path = PathBuf::from(exec_path);

    // Get the libexec directory (parent of git-core)
    let libexec_target = exec_path.parent().ok_or_else(|| {
        GitAiError::Generic("Cannot get libexec directory from exec-path".to_string())
    })?;

    // Create symlink: base_dir/libexec -> /usr/libexec
    let symlink_path = base_dir.join("libexec");

    // Remove existing symlink/junction/directory if present
    if symlink_path.exists() || symlink_path.symlink_metadata().is_ok() {
        #[cfg(windows)]
        {
            // Try remove_dir (works for empty dirs and junctions), then remove_dir_all
            // (works for copied directories), then remove_file (for file symlinks).
            if std::fs::remove_dir(&symlink_path).is_err()
                && std::fs::remove_dir_all(&symlink_path).is_err()
            {
                let _ = std::fs::remove_file(&symlink_path);
            }
        }
        #[cfg(unix)]
        std::fs::remove_file(&symlink_path)?;
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(libexec_target, &symlink_path)?;

    #[cfg(windows)]
    {
        if create_junction(&symlink_path, libexec_target).is_err() {
            copy_dir_recursive(libexec_target, &symlink_path)?;
        }
    }

    Ok(())
}

/// Create a directory junction on Windows (doesn't require admin privileges)
#[cfg(windows)]
fn create_junction(
    junction_path: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), GitAiError> {
    use std::process::Command;

    // Use mklink /J to create a junction - this doesn't require admin privileges
    let status = Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &junction_path.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .output()
        .map_err(|e| GitAiError::Generic(format!("Failed to run mklink: {}", e)))?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(GitAiError::Generic(format!(
            "Failed to create junction: {}",
            stderr
        )));
    }

    Ok(())
}

#[cfg(windows)]
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), GitAiError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let entry_path = entry.path();
        let dest_path = dst.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_recursive(&entry_path, &dest_path)?;
        } else {
            std::fs::copy(&entry_path, &dest_path)?;
        }
    }
    Ok(())
}
