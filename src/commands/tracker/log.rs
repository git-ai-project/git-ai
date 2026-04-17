use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

pub enum LogStatus {
    Uploaded,
    Skipped,
    Failed,
    RetryOk,
}

impl LogStatus {
    fn as_str(&self) -> &str {
        match self {
            LogStatus::Uploaded => "uploaded",
            LogStatus::Skipped => "skipped",
            LogStatus::Failed => "failed",
            LogStatus::RetryOk => "retry_ok",
        }
    }

    fn display_name(&self) -> &str {
        match self {
            LogStatus::Uploaded => "✓ 上报成功",
            LogStatus::Skipped => "⊘ 已跳过",
            LogStatus::Failed => "✗ 上报失败",
            LogStatus::RetryOk => "↻ 重试成功",
        }
    }
}

fn reason_display(reason: &str) -> String {
    match reason {
        "already_reported" => "已上报过".to_string(),
        "blacklisted" => "黑名单过滤".to_string(),
        "merge_commit" => "合并提交".to_string(),
        "synthetic_message" => "自动生成的提交信息".to_string(),
        "copy_paste_threshold" => "手动添加代码超过阈值（>300行）".to_string(),
        other => other.to_string(),
    }
}

pub fn log_path() -> PathBuf {
    crate::mdm::utils::home_dir()
        .join(".git-ai")
        .join("tracker-upload.log")
}

pub fn append_log(
    status: LogStatus,
    commit_sha: &str,
    remote: &str,
    branch: &str,
    repo_path: &str,
    reason: Option<&str>,
) {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let sha7 = &commit_sha[..commit_sha.len().min(7)];
    let remote_branch = format!("{}/{}", remote, branch);

    let line = if let Some(r) = reason {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            timestamp,
            status.as_str(),
            sha7,
            remote_branch,
            repo_path,
            r
        )
    } else {
        format!(
            "{}\t{}\t{}\t{}\t{}\n",
            timestamp,
            status.as_str(),
            sha7,
            remote_branch,
            repo_path
        )
    };

    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
}

pub fn print_log(lines: usize) {
    let path = log_path();
    if !path.exists() {
        println!("暂无上报日志");
        return;
    }

    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("无法打开日志文件: {}", e);
            return;
        }
    };

    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    let start = if all_lines.len() > lines {
        all_lines.len() - lines
    } else {
        0
    };

    for line in &all_lines[start..] {
        print_formatted_line(line);
    }
}

fn print_formatted_line(line: &str) {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < 5 {
        println!("{}", line);
        return;
    }

    let timestamp = parts[0];
    let status_str = parts[1];
    let sha = parts[2];
    let remote_branch = parts[3];
    let repo_path = parts[4];
    let reason = if parts.len() > 5 {
        Some(parts[5])
    } else {
        None
    };

    let time_display = if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        let local: chrono::DateTime<chrono::Local> = dt.into();
        local.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        timestamp.to_string()
    };

    let status_display = match status_str {
        "uploaded" => LogStatus::Uploaded.display_name(),
        "skipped" => LogStatus::Skipped.display_name(),
        "failed" => LogStatus::Failed.display_name(),
        "retry_ok" => LogStatus::RetryOk.display_name(),
        _ => status_str,
    };

    let repo_name = repo_path
        .trim_end_matches("/.git")
        .rsplit('/')
        .next()
        .unwrap_or(repo_path);

    print!(
        "{} {} {} {}/{}",
        time_display, status_display, sha, repo_name, remote_branch
    );

    if let Some(r) = reason {
        let reason_text = reason_display(r);
        print!(" - {}", reason_text);
    }

    println!();
}
