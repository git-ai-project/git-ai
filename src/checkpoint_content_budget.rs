use std::fmt::Display;

use crate::config;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckpointContentBudget {
    max_file_size_bytes: usize,
    max_total_size_bytes: usize,
    max_total_lines: usize,
    used_size_bytes: usize,
    used_lines: usize,
}

impl CheckpointContentBudget {
    pub(crate) fn from_config(config: &config::Config) -> Self {
        Self {
            max_file_size_bytes: config.max_checkpoint_file_size_bytes(),
            max_total_size_bytes: config.max_checkpoint_total_size_bytes(),
            max_total_lines: config.max_checkpoint_total_lines(),
            used_size_bytes: 0,
            used_lines: 0,
        }
    }

    pub(crate) fn max_file_size_bytes(&self) -> usize {
        self.max_file_size_bytes
    }

    pub(crate) fn reserve(&mut self, path: impl Display, content: &str) -> bool {
        let size_bytes = content.len();
        if size_bytes > self.max_file_size_bytes {
            tracing::warn!(
                "skipping file larger than max_checkpoint_file_size_bytes: {} ({} bytes)",
                path,
                size_bytes,
            );
            return false;
        }

        let line_count = checkpoint_content_line_count(content);
        if self.used_size_bytes.saturating_add(size_bytes) > self.max_total_size_bytes {
            tracing::warn!(
                "skipping file over max_checkpoint_total_size_bytes budget: {} ({} bytes, {} bytes already used, {} bytes max)",
                path,
                size_bytes,
                self.used_size_bytes,
                self.max_total_size_bytes,
            );
            return false;
        }
        if self.used_lines.saturating_add(line_count) > self.max_total_lines {
            tracing::warn!(
                "skipping file over max_checkpoint_total_lines budget: {} ({} lines, {} lines already used, {} lines max)",
                path,
                line_count,
                self.used_lines,
                self.max_total_lines,
            );
            return false;
        }

        self.used_size_bytes += size_bytes;
        self.used_lines += line_count;
        true
    }
}

fn checkpoint_content_line_count(content: &str) -> usize {
    if content.is_empty() {
        return 0;
    }
    content.as_bytes().iter().filter(|&&b| b == b'\n').count()
        + usize::from(!content.as_bytes().ends_with(b"\n"))
}
