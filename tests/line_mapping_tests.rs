/// Test cases for reverse line-number mapping through checkpoint diffs.
///
/// Each test creates an "old" file and a "new" file by applying specific edits,
/// then extracts added_ranges/deleted_ranges, and verifies the expected mapping
/// from new-file line numbers to old-file line numbers.

/// Represents the diff between two file states, as stored in change_history
#[derive(Debug, Clone)]
struct DiffRanges {
    added: Vec<(u32, u32)>,   // in new-file coordinates (1-based, inclusive)
    deleted: Vec<(u32, u32)>, // in old-file coordinates (1-based, inclusive)
}

/// Apply edits to an old file to produce a new file, and extract the diff ranges.
/// `edits` is a list of (action, old_line_start, old_line_end, new_content).
/// Instead of specifying edits, we just provide old and new content and diff them.
fn diff_lines(old_lines: &[&str], new_lines: &[&str]) -> DiffRanges {
    // Use a simple LCS-based diff to find added and deleted ranges
    let old: Vec<&str> = old_lines.to_vec();
    let new: Vec<&str> = new_lines.to_vec();

    // Build edit script using Myers-like approach via similar crate logic
    // For test purposes, use a simple approach: find matching lines
    let ops = compute_diff_ops(&old, &new);

    let mut added = Vec::new();
    let mut deleted = Vec::new();
    let mut old_pos = 1u32;
    let mut new_pos = 1u32;

    for op in &ops {
        match op {
            DiffOp::Equal(n) => {
                old_pos += *n as u32;
                new_pos += *n as u32;
            }
            DiffOp::Delete(n) => {
                let start = old_pos;
                let end = old_pos + *n as u32 - 1;
                deleted.push((start, end));
                old_pos += *n as u32;
            }
            DiffOp::Insert(n) => {
                let start = new_pos;
                let end = new_pos + *n as u32 - 1;
                added.push((start, end));
                new_pos += *n as u32;
            }
        }
    }

    DiffRanges { added, deleted }
}

#[derive(Debug, Clone)]
enum DiffOp {
    Equal(usize),
    Delete(usize),
    Insert(usize),
}

/// Simple LCS-based diff
fn compute_diff_ops(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    let m = old.len();
    let n = new.len();

    // Build LCS table
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to get edit script
    let mut raw_ops = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            raw_ops.push(DiffOp::Equal(1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            raw_ops.push(DiffOp::Insert(1));
            j -= 1;
        } else {
            raw_ops.push(DiffOp::Delete(1));
            i -= 1;
        }
    }
    raw_ops.reverse();

    // Merge consecutive same-type ops
    let mut merged = Vec::new();
    for op in raw_ops {
        match (merged.last_mut(), &op) {
            (Some(DiffOp::Equal(n)), DiffOp::Equal(1)) => *n += 1,
            (Some(DiffOp::Delete(n)), DiffOp::Delete(1)) => *n += 1,
            (Some(DiffOp::Insert(n)), DiffOp::Insert(1)) => *n += 1,
            _ => merged.push(op),
        }
    }
    merged
}

/// THE FUNCTION UNDER TEST: reconstruct diff ops from add/delete ranges,
/// then map a new-file line to its old-file line (or None if inserted).
fn map_new_to_old(new_line: u32, ranges: &DiffRanges) -> Option<u32> {
    // Step 1: Reconstruct diff ops from ranges
    let ops = reconstruct_diff_ops(&ranges.added, &ranges.deleted);

    // Step 2: Walk ops to find the mapping
    let mut old_pos = 1u32;
    let mut new_pos = 1u32;

    for op in &ops {
        match op {
            DiffOp::Equal(n) => {
                let n = *n as u32;
                if new_line >= new_pos && new_line < new_pos + n {
                    return Some(old_pos + (new_line - new_pos));
                }
                old_pos += n;
                new_pos += n;
            }
            DiffOp::Insert(n) => {
                let n = *n as u32;
                if new_line >= new_pos && new_line < new_pos + n {
                    return None; // line was inserted
                }
                new_pos += n;
            }
            DiffOp::Delete(n) => {
                old_pos += *n as u32;
            }
        }
    }
    // Past all ops: trailing unchanged lines
    Some(old_pos + (new_line - new_pos))
}

fn reconstruct_diff_ops(
    added: &[(u32, u32)],
    deleted: &[(u32, u32)],
) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let mut old_pos = 1u32;
    let mut new_pos = 1u32;
    let mut add_idx = 0usize;
    let mut del_idx = 0usize;

    loop {
        let next_del_start = deleted.get(del_idx).map(|(s, _)| *s);
        let next_add_start = added.get(add_idx).map(|(s, _)| *s);

        match (next_del_start, next_add_start) {
            (None, None) => break,
            (Some(del_start), None) => {
                let equal = del_start - old_pos;
                if equal > 0 {
                    ops.push(DiffOp::Equal(equal as usize));
                    old_pos += equal;
                    new_pos += equal;
                }
                let del_count = deleted[del_idx].1 - deleted[del_idx].0 + 1;
                ops.push(DiffOp::Delete(del_count as usize));
                old_pos += del_count;
                del_idx += 1;
            }
            (None, Some(add_start)) => {
                let equal = add_start - new_pos;
                if equal > 0 {
                    ops.push(DiffOp::Equal(equal as usize));
                    old_pos += equal;
                    new_pos += equal;
                }
                let add_count = added[add_idx].1 - added[add_idx].0 + 1;
                ops.push(DiffOp::Insert(add_count as usize));
                new_pos += add_count;
                add_idx += 1;
            }
            (Some(del_start), Some(add_start)) => {
                let gap_to_del = del_start - old_pos;
                let gap_to_add = add_start - new_pos;
                let equal = gap_to_del.min(gap_to_add);
                if equal > 0 {
                    ops.push(DiffOp::Equal(equal as usize));
                    old_pos += equal;
                    new_pos += equal;
                }
                if old_pos == del_start {
                    let del_count = deleted[del_idx].1 - deleted[del_idx].0 + 1;
                    ops.push(DiffOp::Delete(del_count as usize));
                    old_pos += del_count;
                    del_idx += 1;
                }
                if new_pos == add_start {
                    let add_count = added[add_idx].1 - added[add_idx].0 + 1;
                    ops.push(DiffOp::Insert(add_count as usize));
                    new_pos += add_count;
                    add_idx += 1;
                }
            }
        }
    }

    ops
}

/// Build the ground-truth mapping by diffing old and new content directly
fn ground_truth_mapping(old_lines: &[&str], new_lines: &[&str]) -> Vec<Option<u32>> {
    let ops = compute_diff_ops(old_lines, new_lines);
    let mut mapping = Vec::new(); // index 0 = new line 1
    let mut old_pos = 1u32;

    for op in &ops {
        match op {
            DiffOp::Equal(n) => {
                for i in 0..*n {
                    mapping.push(Some(old_pos + i as u32));
                }
                old_pos += *n as u32;
            }
            DiffOp::Insert(n) => {
                for _ in 0..*n {
                    mapping.push(None);
                }
            }
            DiffOp::Delete(n) => {
                old_pos += *n as u32;
            }
        }
    }
    mapping
}

// ============================================================================
// TEST CASES
// ============================================================================

#[test]
fn test_case_1_simple_deletion() {
    // Delete lines 3-4 from the middle
    let old = vec!["a", "b", "c", "d", "e", "f"];
    let new = vec!["a", "b", "e", "f"];

    let ranges = diff_lines(&old, &new);
    println!("Case 1 - Simple deletion");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth: {:?}", truth);

    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        println!("  new_line {} -> {:?} (expected {:?})", new_line, result, expected);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_2_simple_insertion() {
    // Insert 2 new lines after line 2
    let old = vec!["a", "b", "e", "f"];
    let new = vec!["a", "b", "c", "d", "e", "f"];

    let ranges = diff_lines(&old, &new);
    println!("Case 2 - Simple insertion");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth: {:?}", truth);

    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        println!("  new_line {} -> {:?} (expected {:?})", new_line, result, expected);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_3_replacement_same_size() {
    // Replace lines 3-4 with 2 different lines (same count)
    let old = vec!["a", "b", "c", "d", "e", "f"];
    let new = vec!["a", "b", "X", "Y", "e", "f"];

    let ranges = diff_lines(&old, &new);
    println!("Case 3 - Replacement (same size)");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth: {:?}", truth);

    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        println!("  new_line {} -> {:?} (expected {:?})", new_line, result, expected);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_4_replacement_grow() {
    // Replace 2 lines with 4 lines (net +2)
    let old = vec!["a", "b", "c", "d", "e", "f"];
    let new = vec!["a", "b", "W", "X", "Y", "Z", "e", "f"];

    let ranges = diff_lines(&old, &new);
    println!("Case 4 - Replacement (grow: 2 deleted, 4 added)");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth: {:?}", truth);

    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        println!("  new_line {} -> {:?} (expected {:?})", new_line, result, expected);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_5_replacement_shrink() {
    // Replace 4 lines with 2 lines (net -2)
    let old = vec!["a", "b", "c", "d", "e", "f", "g", "h"];
    let new = vec!["a", "b", "X", "Y", "g", "h"];

    let ranges = diff_lines(&old, &new);
    println!("Case 5 - Replacement (shrink: 4 deleted, 2 added)");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth: {:?}", truth);

    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        println!("  new_line {} -> {:?} (expected {:?})", new_line, result, expected);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_6_the_tricky_case_delete_then_insert_at_different_positions() {
    // Delete lines 4-6, then insert 3 new lines later
    // This is the case that breaks the naive formula:
    // deleted in old coords != added in new coords
    let old = vec![
        "L01", "L02", "L03", "OLD4", "OLD5", "OLD6", "L07", "L08", "L09", "L10",
    ];
    let new = vec![
        "L01", "L02", "L03", "L07", "L08", "NEW1", "NEW2", "NEW3", "L09", "L10",
    ];

    let ranges = diff_lines(&old, &new);
    println!("Case 6 - Delete then insert at different positions");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);
    // Expected: deleted [(4,6)] in old coords, added [(6,8)] in new coords

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth: {:?}", truth);

    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        println!("  new_line {} -> {:?} (expected {:?})", new_line, result, expected);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_7_user_scenario_delete_18_22_insert_20_24() {
    // The exact scenario from the user's question:
    // Old file: 25 lines. Delete old lines 18-22. Insert new lines 20-24.
    let mut old: Vec<&str> = Vec::new();
    for i in 1..=25 {
        // We need static strings, use a fixed set
        old.push(match i {
            1 => "L01", 2 => "L02", 3 => "L03", 4 => "L04", 5 => "L05",
            6 => "L06", 7 => "L07", 8 => "L08", 9 => "L09", 10 => "L10",
            11 => "L11", 12 => "L12", 13 => "L13", 14 => "L14", 15 => "L15",
            16 => "L16", 17 => "L17", 18 => "OLD18", 19 => "OLD19", 20 => "OLD20",
            21 => "OLD21", 22 => "OLD22", 23 => "L23", 24 => "L24", 25 => "L25",
            _ => unreachable!(),
        });
    }

    // New file: lines 1-17 unchanged, old 18-22 deleted, old 23-24 shift to 18-19,
    // then 5 new lines at 20-24, then old 25 at 25
    let new = vec![
        "L01", "L02", "L03", "L04", "L05", "L06", "L07", "L08", "L09", "L10",
        "L11", "L12", "L13", "L14", "L15", "L16", "L17",
        "L23", "L24",           // old 23-24 shifted to new 18-19
        "NEW20", "NEW21", "NEW22", "NEW23", "NEW24", // inserted at new 20-24
        "L25",                  // old 25 at new 25
    ];

    assert_eq!(new.len(), 25); // 25 - 5 deleted + 5 added = 25

    let ranges = diff_lines(&old, &new);
    println!("Case 7 - User scenario: delete old 18-22, insert new 20-24");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth mapping (new_line -> old_line):");
    for (i, old_line) in truth.iter().enumerate() {
        let new_line = i + 1;
        let new_content = new[i];
        println!(
            "    new {:>2} ({:>6}) -> old {:?}",
            new_line, new_content, old_line
        );
    }

    println!("\n  Testing map_new_to_old:");
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        let status = if result == *expected { "OK" } else { "FAIL" };
        println!(
            "    new {:>2} -> got {:?}, expected {:?}  [{}]",
            new_line, result, expected, status
        );
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_8_multiple_edits_scattered() {
    // Multiple separate edits across the file
    // Delete line 2, insert after line 3, delete lines 7-8, insert at end
    let old = vec![
        "a", "DEL", "b", "c", "d", "e", "DEL2", "DEL3", "f", "g",
    ];
    let new = vec![
        "a", "b", "NEW1", "NEW2", "c", "d", "e", "f", "g", "NEW3",
    ];

    let ranges = diff_lines(&old, &new);
    println!("Case 8 - Multiple scattered edits");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth: {:?}", truth);

    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        println!(
            "  new {:>2} ({:>4}) -> {:?} (expected {:?})",
            new_line, new[i], result, expected
        );
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_9_delete_at_start() {
    let old = vec!["DEL1", "DEL2", "a", "b", "c"];
    let new = vec!["a", "b", "c"];

    let ranges = diff_lines(&old, &new);
    println!("Case 9 - Delete at start");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_10_insert_at_start() {
    let old = vec!["a", "b", "c"];
    let new = vec!["NEW1", "NEW2", "a", "b", "c"];

    let ranges = diff_lines(&old, &new);
    println!("Case 10 - Insert at start");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_11_delete_at_end() {
    let old = vec!["a", "b", "c", "DEL1", "DEL2"];
    let new = vec!["a", "b", "c"];

    let ranges = diff_lines(&old, &new);
    println!("Case 11 - Delete at end");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_12_insert_at_end() {
    let old = vec!["a", "b", "c"];
    let new = vec!["a", "b", "c", "NEW1", "NEW2"];

    let ranges = diff_lines(&old, &new);
    println!("Case 12 - Insert at end");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_13_complete_replacement() {
    // Every line changed
    let old = vec!["a", "b", "c"];
    let new = vec!["X", "Y", "Z", "W"];

    let ranges = diff_lines(&old, &new);
    println!("Case 13 - Complete replacement");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_14_no_changes() {
    let old = vec!["a", "b", "c", "d"];
    let new = vec!["a", "b", "c", "d"];

    let ranges = diff_lines(&old, &new);
    println!("Case 14 - No changes");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_15_alternating_delete_insert() {
    // Delete a line, keep one, insert one, keep one, delete two - complex interleaving
    let old = vec!["DEL", "keep1", "keep2", "DEL2", "DEL3", "keep3"];
    let new = vec!["keep1", "NEW", "keep2", "keep3", "NEW2"];

    let ranges = diff_lines(&old, &new);
    println!("Case 15 - Alternating delete/insert");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    println!("  Ground truth: {:?}", truth);

    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        println!(
            "  new {:>2} ({:>5}) -> {:?} (expected {:?})",
            new_line, new[i], result, expected
        );
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_16_new_file_from_scratch() {
    // Old file is empty (or doesn't exist), everything is added
    let old: Vec<&str> = vec![];
    let new = vec!["a", "b", "c"];

    let ranges = diff_lines(&old, &new);
    println!("Case 16 - New file from scratch");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}

#[test]
fn test_case_17_file_deleted_entirely() {
    // New file is empty, everything is deleted
    let old = vec!["a", "b", "c"];
    let new: Vec<&str> = vec![];

    let ranges = diff_lines(&old, &new);
    println!("Case 17 - File deleted entirely");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    assert!(truth.is_empty()); // no lines in new file to map
}

#[test]
fn test_case_18_single_line_change() {
    // Only one line differs
    let old = vec!["a", "b", "c", "d", "e"];
    let new = vec!["a", "b", "X", "d", "e"];

    let ranges = diff_lines(&old, &new);
    println!("Case 18 - Single line change");
    println!("  deleted: {:?}, added: {:?}", ranges.deleted, ranges.added);

    let truth = ground_truth_mapping(&old, &new);
    for (i, expected) in truth.iter().enumerate() {
        let new_line = (i + 1) as u32;
        let result = map_new_to_old(new_line, &ranges);
        assert_eq!(result, *expected, "Mismatch at new_line {}", new_line);
    }
}
