pub mod config;
pub mod notes;
pub mod filter;
pub mod diff;
pub mod upload;
pub mod retry;

use std::collections::HashMap;

pub fn report_pushed_commits(
    repo_path: &str,
    pre_push_refs: &HashMap<String, String>,
    remote: &str,
) {
    let _ = (repo_path, pre_push_refs, remote);
}
