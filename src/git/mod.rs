pub mod cli_parser;
pub mod diff_tree_to_tree;
pub mod refs;
pub mod repository;

pub mod authorship_traversal;

#[allow(unused_imports)]
pub use repository::{
    find_repository, find_repository_for_file, find_repository_in_path, from_bare_repository,
    from_non_bare_repository, group_files_by_repository,
};
pub mod repo_storage;
pub mod rewrite_log;
pub mod status;
pub mod sync_authorship;

#[cfg(feature = "test-support")]
pub mod test_utils;
