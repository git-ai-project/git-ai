#[macro_use]
mod repos;

use repos::graphite_test_harness::skip_unless_gt;
use repos::test_repo::TestRepo;

#[test]
#[ignore]
fn test_gt_submit_sync_get_merge_pr_unlink_are_optional_auth_flows() {
    if skip_unless_gt() {
        return;
    }

    let repo = TestRepo::new();
    let mut file = repo.filename("auth.txt");
    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    repo.gt(&["init", "--trunk", "main"]).unwrap();

    // These flows intentionally remain optional because they require
    // Graphite/GitHub auth and network setup.
    let _ = repo.gt(&["submit", "--dry-run"]);
    let _ = repo.gt(&["sync"]);
    let _ = repo.gt(&["get"]);
    let _ = repo.gt(&["merge", "--dry-run"]);
    let _ = repo.gt(&["pr"]);
    let _ = repo.gt(&["unlink"]);
}
