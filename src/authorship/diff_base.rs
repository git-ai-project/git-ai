pub(crate) const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Resolve the diff base for post-commit diff parsing so the diff is always
/// bounded to the single commit being finalized.
///
/// The caller's `parent_sha` is normally the immediate parent already, but on
/// the daemon's fast-forward `update-ref` path it can be the old branch tip from
/// before a pull. Using `<commit_sha>^` lets Git resolve the finalized commit's
/// first parent inside the existing diff spawn. Root commits use Git's empty
/// tree hash because there is no parent revision.
pub(crate) fn single_commit_diff_base(parent_sha: &str, commit_sha: &str) -> String {
    if parent_sha == "initial" {
        EMPTY_TREE_SHA.to_string()
    } else {
        format!("{commit_sha}^")
    }
}
