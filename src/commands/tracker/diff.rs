use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;
use std::process::Command;

const CODE_EXTENSIONS: &[&str] = &[
    ".rs", ".py", ".js", ".ts", ".tsx", ".jsx", ".go", ".java", ".c", ".cpp", ".h", ".hpp", ".rb",
    ".php", ".swift", ".kt", ".scala", ".sh", ".sql", ".css", ".html", ".vue", ".svelte",
];

pub fn collect_code_diff(repo_path: &str, commit_sha: &str) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(["-C", repo_path, "show", "--format=", commit_sha])
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let diff = String::from_utf8_lossy(&output.stdout);
    let filtered = filter_code_only(&diff);
    let truncated = truncate_to_100kb(&filtered);

    gzip_compress(&truncated)
}

fn filter_code_only(diff: &str) -> String {
    let mut result = String::new();
    let mut in_code_file = false;

    for line in diff.lines() {
        if line.starts_with("diff --git") {
            in_code_file = CODE_EXTENSIONS.iter().any(|ext| line.contains(ext));
        }
        if in_code_file {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

fn truncate_to_100kb(text: &str) -> String {
    const MAX_BYTES: usize = 100 * 1024;
    if text.len() <= MAX_BYTES {
        text.to_string()
    } else {
        text.chars().take(MAX_BYTES).collect()
    }
}

fn gzip_compress(data: &str) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data.as_bytes())
        .map_err(|e| e.to_string())?;
    encoder.finish().map_err(|e| e.to_string())
}
