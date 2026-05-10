use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::rebase_types::DiffHunk;
use std::collections::HashMap;

/// Parse a unified diff hunk header line like `@@ -10,5 +12,6 @@ context`
/// Returns None if parsing fails.
pub fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 || parts[0] != "@@" {
        return None;
    }

    let old_part = parts[1].trim_start_matches('-');
    let new_part = parts[2].trim_start_matches('+');

    let (old_start, old_count) = parse_range_spec(old_part)?;
    let (new_start, new_count) = parse_range_spec(new_part)?;

    Some(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        added_lines: Vec::new(),
    })
}

/// Parse a range spec like "10,5" or "10" (count defaults to 1, but "10,0" means 0).
pub fn parse_range_spec(spec: &str) -> Option<(u32, u32)> {
    if let Some((start_str, count_str)) = spec.split_once(',') {
        let start = start_str.parse().ok()?;
        let count = count_str.parse().ok()?;
        Some((start, count))
    } else {
        let start = spec.parse().ok()?;
        Some((start, 1))
    }
}

/// Apply hunk-based line offset adjustments to existing line attributions.
///
/// Instead of re-diffing file contents, this uses pre-computed hunk information from
/// `git diff-tree -p -U0` to adjust attribution line numbers. For each hunk:
/// - Lines before the hunk: keep at same position (with accumulated offset)
/// - Lines in a deletion region: dropped (those lines were removed)
/// - Lines after the hunk: shifted by the net offset (new_count - old_count)
///
/// This is O(attrs + hunks) instead of O(file_length) for the full diff approach.
pub fn apply_hunks_to_line_attributions(
    old_attrs: &[LineAttribution],
    hunks: &[DiffHunk],
) -> Vec<LineAttribution> {
    if hunks.is_empty() {
        return old_attrs.to_vec();
    }

    // Build preserved segments: ranges of old line numbers that survive and their offset.
    // Between hunks, lines are preserved with an accumulated offset.
    let mut segments: Vec<(u32, u32, i64)> = Vec::with_capacity(hunks.len() + 1);
    let mut offset: i64 = 0;
    let mut prev_old_end: u32 = 1; // 1-indexed

    for hunk in hunks {
        // Preserved segment before this hunk
        if prev_old_end < hunk.old_start + 1 {
            // Lines from prev_old_end to hunk.old_start are preserved
            // For pure insertions (old_count=0), old_start points to the line AFTER which
            // insertion happens, so lines up to and including old_start are preserved
            let seg_end = if hunk.old_count == 0 {
                hunk.old_start // inclusive
            } else {
                hunk.old_start.saturating_sub(1) // up to but not including the hunk
            };
            if prev_old_end <= seg_end {
                segments.push((prev_old_end, seg_end, offset));
            }
        }

        // The hunk itself: old lines old_start..old_start+old_count-1 are deleted/replaced.
        // No segment for these lines (they're removed).
        // For pure insertion (old_count=0): no lines are removed, but offset changes.

        offset += hunk.new_count as i64 - hunk.old_count as i64;

        if hunk.old_count == 0 {
            prev_old_end = hunk.old_start + 1; // after the insertion point
        } else {
            prev_old_end = hunk.old_start + hunk.old_count; // after the deleted range
        }
    }

    // Final segment after last hunk (up to a very large line number)
    segments.push((prev_old_end, u32::MAX, offset));

    // Apply the mapping to each attribution
    let mut new_attrs: Vec<LineAttribution> = Vec::with_capacity(old_attrs.len());

    for attr in old_attrs {
        // For each attribution range, find the preserved segments that overlap
        for &(seg_start, seg_end, seg_offset) in &segments {
            let range_start = attr.start_line.max(seg_start);
            let range_end = attr.end_line.min(seg_end);

            if range_start <= range_end {
                let new_start = (range_start as i64 + seg_offset).max(1) as u32;
                let new_end = (range_end as i64 + seg_offset).max(1) as u32;
                new_attrs.push(LineAttribution {
                    start_line: new_start,
                    end_line: new_end,
                    author_id: attr.author_id.clone(),
                    overrode: attr.overrode.clone(),
                });
            }
        }
    }

    new_attrs
}

/// Overlay a new attribution range onto an existing sorted attribution list.
/// Removes or splits any existing attributions that overlap the new range,
/// then inserts the new attribution.
pub fn overlay_attribution(
    attrs: &mut Vec<LineAttribution>,
    start: u32,
    end: u32,
    author_id: String,
) {
    // Remove overlapping entries, splitting partial overlaps.
    let mut i = 0;
    let mut to_insert_after: Vec<LineAttribution> = Vec::new();
    while i < attrs.len() {
        let a = &attrs[i];
        if a.end_line < start || a.start_line > end {
            // No overlap.
            i += 1;
            continue;
        }
        // Overlap detected — remove and potentially split.
        let removed = attrs.remove(i);
        if removed.start_line < start {
            // Left fragment survives.
            attrs.insert(
                i,
                LineAttribution {
                    start_line: removed.start_line,
                    end_line: start - 1,
                    author_id: removed.author_id.clone(),
                    overrode: removed.overrode.clone(),
                },
            );
            i += 1;
        }
        if removed.end_line > end {
            // Right fragment survives — defer insertion to maintain order.
            to_insert_after.push(LineAttribution {
                start_line: end + 1,
                end_line: removed.end_line,
                author_id: removed.author_id,
                overrode: removed.overrode,
            });
        }
        // Don't increment i — next element shifted into this position.
    }
    for frag in to_insert_after {
        attrs.push(frag);
    }

    // Insert the new attribution.
    attrs.push(LineAttribution {
        start_line: start,
        end_line: end,
        author_id,
        overrode: None,
    });
}

/// Remap the base_commit_sha field in a serialized authorship note to point to a new commit.
/// Tries fast string replacement first, falls back to full deserialization.
pub fn remap_note_content_for_target_commit(note_content: &str, target_commit: &str) -> String {
    if let Some(remapped_note) = try_remap_base_commit_sha_field(note_content, target_commit) {
        return remapped_note;
    }

    if let Ok(mut authorship_log) = AuthorshipLog::deserialize_from_string(note_content) {
        authorship_log.metadata.base_commit_sha = target_commit.to_string();
        if let Ok(serialized) = authorship_log.serialize_to_string() {
            return serialized;
        }
    }
    note_content.to_string()
}

/// Fast-path string replacement for the base_commit_sha JSON field without full deserialization.
pub fn try_remap_base_commit_sha_field(note_content: &str, target_commit: &str) -> Option<String> {
    let field = "\"base_commit_sha\"";
    let field_pos = note_content.find(field)?;
    let bytes = note_content.as_bytes();

    let mut pos = field_pos + field.len();
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b':' {
        return None;
    }
    pos += 1;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\n' | b'\t' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return None;
    }
    pos += 1;
    let value_start = pos;

    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => {
                pos += 2;
            }
            b'"' => {
                let value_end = pos;
                let mut remapped = String::with_capacity(
                    note_content.len() - (value_end - value_start) + target_commit.len(),
                );
                remapped.push_str(&note_content[..value_start]);
                remapped.push_str(target_commit);
                remapped.push_str(&note_content[value_end..]);
                return Some(remapped);
            }
            _ => {
                pos += 1;
            }
        }
    }

    None
}

/// Transfer line attributions from old file content to new file content using line-level diffing.
/// This replaces the blame-based slow path by using imara-diff to compute how lines moved
/// between the old and new versions, then carrying attributions forward positionally.
///
/// - Equal lines: carry the original attribution forward
/// - Inserted lines: no attribution (new content)
/// - Deleted lines: dropped
/// - Replaced lines: no attribution (content changed)
#[doc(hidden)]
pub fn diff_based_line_attribution_transfer(
    old_content: &str,
    new_content: &str,
    old_line_attrs: &[LineAttribution],
) -> Vec<LineAttribution> {
    use crate::authorship::imara_diff_utils::{DiffOp, capture_diff_slices};

    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    // Build a sparse lookup from 0-indexed line position → author_id for old content.
    // Using a HashMap instead of a full-size Vec avoids allocating O(file_size) memory
    // when only a small fraction of lines carry AI attribution.
    let mut old_line_author: HashMap<usize, &str> = HashMap::new();
    for attr in old_line_attrs {
        for line_num in attr.start_line..=attr.end_line {
            let idx = (line_num as usize).saturating_sub(1);
            if idx < old_lines.len() {
                old_line_author.insert(idx, &attr.author_id);
            }
        }
    }

    let diff_ops = capture_diff_slices(&old_lines, &new_lines);

    let mut new_line_attrs: Vec<LineAttribution> = Vec::with_capacity(old_line_author.len());

    for op in &diff_ops {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                // Carry attributions forward for equal lines
                for i in 0..*len {
                    let old_idx = old_index + i;
                    let new_line_num = (new_index + i + 1) as u32;
                    if let Some(author_id) = old_line_author.get(&old_idx) {
                        new_line_attrs.push(LineAttribution {
                            start_line: new_line_num,
                            end_line: new_line_num,
                            author_id: author_id.to_string(),
                            overrode: None,
                        });
                    }
                }
            }
            DiffOp::Insert { .. } | DiffOp::Delete { .. } | DiffOp::Replace { .. } => {
                // Insert: new lines, no attribution
                // Delete: old lines removed, nothing to output
                // Replace: content changed, no attribution carried
            }
        }
    }

    new_line_attrs
}

/// Compute new line attributions for a file after content changes.
/// Uses diff-based positional transfer when previous content/attrs are available,
/// otherwise falls back to content-matching from the original_head line→author map.
pub fn compute_line_attrs_for_changed_file(
    new_content: &str,
    old_content: Option<&String>,
    old_attrs: Option<&[LineAttribution]>,
    original_head_line_map: Option<&HashMap<String, String>>,
) -> Vec<LineAttribution> {
    if let (Some(old_c), Some(old_a)) = (old_content, old_attrs) {
        diff_based_line_attribution_transfer(old_c, new_content, old_a)
    } else {
        // No previous content — fall back to content-matching from original_head
        let mut attrs = Vec::new();
        for (line_idx, line_content) in new_content.lines().enumerate() {
            if let Some(author_id) = original_head_line_map.and_then(|m| m.get(line_content)) {
                let line_num = (line_idx + 1) as u32;
                attrs.push(LineAttribution {
                    start_line: line_num,
                    end_line: line_num,
                    author_id: author_id.clone(),
                    overrode: None,
                });
            }
        }
        attrs
    }
}
