use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

#[test]
fn test_ai_reflow_human_single_line_call_is_fully_ai() {
    let repo = TestRepo::new();
    let mut file = repo.filename("call.rs");

    file.set_contents(crate::lines!["call(foo, bar, baz)"]);
    repo.stage_all_and_commit("Initial compact call").unwrap();

    file.set_contents(crate::lines![
        "call(".ai(),
        "  foo,".ai(),
        "  bar,".ai(),
        "  baz".ai(),
        ")".ai(),
    ]);
    repo.stage_all_and_commit("AI reflows call").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "call(".ai(),
        "  foo,".ai(),
        "  bar,".ai(),
        "  baz".ai(),
        ")".ai(),
    ]);
}

#[test]
fn test_ai_indentation_only_change_on_human_block_attributes_touched_line_to_ai() {
    let repo = TestRepo::new();
    let mut file = repo.filename("indent.rs");

    file.set_contents(crate::lines!["fn wrapper() {", "do_work();", "}"]);
    repo.stage_all_and_commit("Initial human block").unwrap();

    file.replace_at(1, "    do_work();".ai());
    repo.stage_all_and_commit("AI reindents body line").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn wrapper() {".human(),
        "    do_work();".ai(),
        "}".human()
    ]);
}

#[test]
fn test_ai_wraps_mixed_human_ai_human_block_all_reformatted_lines_become_ai() {
    let repo = TestRepo::new();
    let mut file = repo.filename("wrapped.rs");

    file.set_contents(crate::lines!["if (ready) {", "do_work();".ai(), "}"]);
    repo.stage_all_and_commit("Initial mixed block").unwrap();

    file.set_contents(crate::lines![
        "fn run() {".ai(),
        "    if (ready) {".ai(),
        "        do_work();".ai(),
        "    }".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("AI wraps and reformats block")
        .unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn run() {".ai(),
        "    if (ready) {".ai(),
        "        do_work();".ai(),
        "    }".ai(),
        "}".human(),
    ]);
}

#[test]
fn test_ai_non_substantial_reflow_with_blank_lines_attributes_blank_and_reflowed_lines_to_ai() {
    let repo = TestRepo::new();
    let mut file = repo.filename("main.rs");

    file.set_contents(crate::lines!["fn main(){println!(\"x\");}"]);
    repo.stage_all_and_commit("Initial compact function")
        .unwrap();

    file.set_contents(crate::lines![
        "fn main() {".ai(),
        "    println!(\"x\");".ai(),
        "".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("AI reformats function").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "fn main() {".ai(),
        "    println!(\"x\");".ai(),
        "".ai(),
        "}".ai(),
    ]);
}

#[test]
fn test_human_on_ai_after_ai_formatting_reclaims_only_human_edited_line() {
    let repo = TestRepo::new();
    let mut file = repo.filename("pipeline.rs");

    file.set_contents(crate::lines!["call(foo, bar, baz)"]);
    repo.stage_all_and_commit("Initial compact call").unwrap();

    file.set_contents(crate::lines![
        "call(".ai(),
        "  foo,".ai(),
        "  bar,".ai(),
        "  baz".ai(),
        ")".ai(),
    ]);
    repo.stage_all_and_commit("AI reflows call").unwrap();

    file.replace_at(2, "  bar_renamed,".human());
    repo.stage_all_and_commit("Human edits one line").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "call(".ai(),
        "  foo,".ai(),
        "  bar_renamed,".human(),
        "  baz".ai(),
        ")".ai(),
    ]);
}

#[test]
fn test_ai_on_human_after_human_edit_reformats_and_takes_line_ownership() {
    let repo = TestRepo::new();
    let mut file = repo.filename("control.rs");

    file.set_contents(crate::lines!["if (enabled) { do_work(); }"]);
    repo.stage_all_and_commit("Initial control flow").unwrap();

    file.replace_at(0, "if (enabled) { run_work(); }".human());
    repo.stage_all_and_commit("Human changes callee").unwrap();

    file.set_contents(crate::lines![
        "if (enabled) {".ai(),
        "    run_work();".ai(),
        "}".ai(),
    ]);
    repo.stage_all_and_commit("AI reformats human change")
        .unwrap();

    file.assert_lines_and_blame(crate::lines![
        "if (enabled) {".ai(),
        "    run_work();".ai(),
        "}".ai(),
    ]);
}

#[test]
fn test_ai_on_ai_second_formatting_pass_keeps_reformatted_region_ai_and_preserves_untouched_human()
{
    let repo = TestRepo::new();
    let mut file = repo.filename("mixed.rs");

    file.set_contents(crate::lines![
        "// header",
        "call(foo,bar,baz)".ai(),
        "// footer"
    ]);
    repo.stage_all_and_commit("Initial mixed ownership")
        .unwrap();

    file.set_contents(crate::lines![
        "// header",
        "call(".ai(),
        "  foo,".ai(),
        "  bar,".ai(),
        "  baz".ai(),
        ")".ai(),
        "// footer",
    ]);
    repo.stage_all_and_commit("AI reflows AI region").unwrap();

    file.assert_lines_and_blame(crate::lines![
        "// header".human(),
        "call(".ai(),
        "  foo,".ai(),
        "  bar,".ai(),
        "  baz".ai(),
        ")".ai(),
        "// footer".human(),
    ]);
}

#[test]
fn test_iterative_human_ai_human_ai_series_assert_each_commit_state() {
    let repo = TestRepo::new();
    let mut file = repo.filename("iterative.py");

    file.set_contents(crate::lines!["items=[1,2,3]", "total=sum(items)"]);
    repo.stage_all_and_commit("Initial script").unwrap();
    file.assert_lines_and_blame(crate::lines![
        "items=[1,2,3]".human(),
        "total=sum(items)".human()
    ]);

    file.replace_at(0, "items = [1, 2, 3]".ai());
    repo.stage_all_and_commit("AI formats list literal")
        .unwrap();
    file.assert_lines_and_blame(crate::lines![
        "items = [1, 2, 3]".ai(),
        "total=sum(items)".human(),
    ]);

    file.replace_at(1, "total = sum(items) + 1".human());
    repo.stage_all_and_commit("Human adjusts total").unwrap();
    file.assert_lines_and_blame(crate::lines![
        "items = [1, 2, 3]".ai(),
        "total = sum(items) + 1".human(),
    ]);

    file.set_contents(crate::lines![
        "items = [".ai(),
        "    1,".ai(),
        "    2,".ai(),
        "    3,".ai(),
        "]".ai(),
        "total = sum(items) + 1".human(),
    ]);
    repo.stage_all_and_commit("AI reflows list vertically")
        .unwrap();
    file.assert_lines_and_blame(crate::lines![
        "items = [".ai(),
        "    1,".ai(),
        "    2,".ai(),
        "    3,".ai(),
        "]".ai(),
        "total = sum(items) + 1".human(),
    ]);
}

#[test]
fn test_multi_file_ai_formatting_commit_tracks_exact_line_blame_in_each_file() {
    let repo = TestRepo::new();
    let mut file_a = repo.filename("a.py");
    let mut file_b = repo.filename("b.toml");

    file_a.set_contents(crate::lines!["result=compute(x,y)"]);
    file_b.set_contents(crate::lines!["[server]", "port=8080"]);
    repo.stage_all_and_commit("Initial multi-file content")
        .unwrap();

    file_a.set_contents(crate::lines![
        "result = compute(".ai(),
        "    x,".ai(),
        "    y,".ai(),
        ")".ai(),
    ]);
    file_b.set_contents(crate::lines!["[server]", "port = 8080".ai()]);
    repo.stage_all_and_commit("AI reformats both files")
        .unwrap();

    file_a.assert_lines_and_blame(crate::lines![
        "result = compute(".ai(),
        "    x,".ai(),
        "    y,".ai(),
        ")".ai(),
    ]);
    file_b.assert_lines_and_blame(crate::lines!["[server]".human(), "port = 8080".ai()]);
}

#[test]
fn test_complex_sectioned_file_ai_formats_only_selected_sections() {
    let repo = TestRepo::new();
    let mut file = repo.filename("settings.ini");

    file.set_contents(crate::lines![
        "[A]", "alpha=1", "", "[B]", "beta=2", "", "[C]", "gamma=3",
    ]);
    repo.stage_all_and_commit("Initial sectioned config")
        .unwrap();

    file.replace_at(1, "alpha = 1".ai());
    file.replace_at(4, "beta = 2".ai());
    repo.stage_all_and_commit("AI formats selected sections")
        .unwrap();

    file.assert_lines_and_blame(crate::lines![
        "[A]".human(),
        "alpha = 1".ai(),
        "".human(),
        "[B]".human(),
        "beta = 2".ai(),
        "".human(),
        "[C]".human(),
        "gamma=3".human(),
    ]);
}

crate::reuse_tests_in_worktree!(
    test_ai_reflow_human_single_line_call_is_fully_ai,
    test_ai_indentation_only_change_on_human_block_attributes_touched_line_to_ai,
    test_ai_wraps_mixed_human_ai_human_block_all_reformatted_lines_become_ai,
    test_ai_non_substantial_reflow_with_blank_lines_attributes_blank_and_reflowed_lines_to_ai,
    test_human_on_ai_after_ai_formatting_reclaims_only_human_edited_line,
    test_ai_on_human_after_human_edit_reformats_and_takes_line_ownership,
    test_ai_on_ai_second_formatting_pass_keeps_reformatted_region_ai_and_preserves_untouched_human,
    test_iterative_human_ai_human_ai_series_assert_each_commit_state,
    test_multi_file_ai_formatting_commit_tracks_exact_line_blame_in_each_file,
    test_complex_sectioned_file_ai_formats_only_selected_sections,
);
