use imara_diff::{Algorithm, Diff, InternedInput, TokenSource};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Attribution {
    pub start: usize,
    pub end: usize,
    pub author_id: String,
    pub ts: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct LineAttribution {
    pub start_line: u32,
    pub end_line: u32,
    pub author_id: String,
    #[serde(default)]
    pub overrode: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// 5-phase algorithm: diff -> catalog -> move detect -> transform -> merge
pub fn update_attributions(
    prev_content: &str,
    new_content: &str,
    prev_attributions: &[Attribution],
    current_author: &str,
    enable_move_detection: bool,
) -> Vec<Attribution> {
    let ts = prev_attributions.iter().map(|a| a.ts).max().unwrap_or(0) + 1;

    let sorted;
    let prev = if !is_sorted(prev_attributions) {
        sorted = sort_attributions(prev_attributions);
        &sorted[..]
    } else {
        prev_attributions
    };

    // Phase 1: Compute byte-level diff ops
    let diff_ops = compute_diff(prev_content, new_content);

    // Phase 2: Build deletion/insertion catalogs
    let (deletions, insertions) = build_catalog(&diff_ops);

    // Phase 3: Detect moves
    let move_mappings = if enable_move_detection && !deletions.is_empty() && !insertions.is_empty()
    {
        detect_moves(prev_content, new_content, &deletions, &insertions)
    } else {
        Vec::new()
    };

    // Phase 4: Transform attributions through the diff
    let transformed = transform_attributions(
        &diff_ops,
        prev,
        current_author,
        ts,
        &insertions,
        &move_mappings,
    );

    // Phase 5: Merge adjacent/overlapping ranges with same metadata
    let merged = merge_attributions(transformed);

    // Phase 6a: For human/known_human checkpoints with no prior state AND no prior
    // content (new file), attribute ALL bytes to the human author. When prev_attributions
    // is empty and prev_content is empty, the entire new file is from the human.
    // When prev_content is NOT empty, Equal bytes represent content from HEAD which
    // should remain unattributed (resolved later via INITIAL from the parent commit's note).
    let is_human_author = current_author.starts_with("h_") || current_author == "human";
    if is_human_author && prev_attributions.is_empty() && prev_content.is_empty() && !new_content.is_empty() {
        return vec![Attribution {
            start: 0,
            end: new_content.len(),
            author_id: current_author.to_string(),
            ts,
        }];
    }

    // Phase 6b: Attribute lines touched by deletions that have no coverage.
    // When a Delete op occurs (AI removes text), the remaining Equal bytes on
    // that line may have no attribution. If prev_attributions was empty (no prior
    // checkpoint for this commit), those lines should be attributed to the current
    // author since the AI semantically modified them.
    // Only applies to AI checkpoints (not human/known_human).
    let is_ai_author = !current_author.starts_with("h_") && current_author != "human";
    if is_ai_author && !deletions.is_empty() && prev_attributions.is_empty() {
        attribute_deletion_touched_lines(&merged, &diff_ops, new_content, current_author, ts)
    } else {
        merged
    }
}

/// Convert char-level attributions to line-level by dominant-author analysis.
pub fn attributions_to_line_attributions(
    content: &str,
    attributions: &[Attribution],
) -> Vec<LineAttribution> {
    if content.is_empty() || attributions.is_empty() {
        return Vec::new();
    }

    let line_ranges = compute_line_ranges(content);
    let line_count = line_ranges.len();
    if line_count == 0 {
        return Vec::new();
    }

    let mut sorted_attrs: Vec<usize> = (0..attributions.len()).collect();
    sorted_attrs.sort_by_key(|&i| (attributions[i].start, attributions[i].end));

    let mut result: Vec<LineAttribution> = Vec::new();
    let mut attr_cursor = 0;

    for (line_idx, &(line_start, line_end)) in line_ranges.iter().enumerate() {
        let line_num = (line_idx + 1) as u32;

        // Advance cursor past attributions that end before this line
        while attr_cursor < sorted_attrs.len()
            && attributions[sorted_attrs[attr_cursor]].end <= line_start
        {
            attr_cursor += 1;
        }

        let line_content = &content[line_start..line_end];
        let is_blank = line_content.chars().all(|c| c.is_whitespace());

        // Find dominant author for this line
        let author = find_dominant_author(
            line_start,
            line_end,
            is_blank,
            &sorted_attrs[attr_cursor..],
            attributions,
            content,
        );

        // Merge with previous LineAttribution if same author
        if let Some(last) = result.last_mut() {
            if last.author_id == author && last.end_line == line_num - 1 {
                last.end_line = line_num;
                continue;
            }
        }
        result.push(LineAttribution {
            start_line: line_num,
            end_line: line_num,
            author_id: author,
            overrode: None,
        });
    }

    result
}

// ---------------------------------------------------------------------------
// Internal diff infrastructure
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOpKind {
    Equal,
    Delete,
    Insert,
}

#[derive(Debug, Clone)]
struct ByteOp {
    kind: DiffOpKind,
    data: Vec<u8>,
}

struct Deletion {
    start: usize,
    end: usize,
}

struct Insertion {
    start: usize,
    end: usize,
}

struct MoveMapping {
    deletion_idx: usize,
    insertion_idx: usize,
    source_range: (usize, usize),
    target_range: (usize, usize),
}

// ---------------------------------------------------------------------------
// Phase 1: Diff computation
// ---------------------------------------------------------------------------

fn compute_diff(old: &str, new: &str) -> Vec<ByteOp> {
    let old_lines = line_slices(old);
    let new_lines = line_slices(new);

    let line_ops = diff_slices(&old_lines, &new_lines);

    let mut result = Vec::new();
    let mut pending: Vec<LineDiffOp> = Vec::new();

    for op in line_ops {
        if matches!(op, LineDiffOp::Equal { .. }) {
            if !pending.is_empty() {
                process_changed_hunk(&pending, &old_lines, &new_lines, old, new, &mut result);
                pending.clear();
            }
            if let LineDiffOp::Equal { old_index, len, .. } = op {
                let start = line_byte_start(&old_lines, old_index);
                let end = line_byte_end(&old_lines, old_index + len, old.len());
                if start < end {
                    result.push(ByteOp {
                        kind: DiffOpKind::Equal,
                        data: old.as_bytes()[start..end].to_vec(),
                    });
                }
            }
        } else {
            pending.push(op);
        }
    }

    if !pending.is_empty() {
        process_changed_hunk(&pending, &old_lines, &new_lines, old, new, &mut result);
    }

    result
}

fn process_changed_hunk(
    ops: &[LineDiffOp],
    old_lines: &[&str],
    new_lines: &[&str],
    old: &str,
    new: &str,
    result: &mut Vec<ByteOp>,
) {
    let (old_start_line, old_end_line) = hunk_bounds(ops, true);
    let (new_start_line, new_end_line) = hunk_bounds(ops, false);

    let old_start = line_byte_start(old_lines, old_start_line);
    let old_end = line_byte_end(old_lines, old_end_line, old.len());
    let new_start = line_byte_start(new_lines, new_start_line);
    let new_end = line_byte_end(new_lines, new_end_line, new.len());

    // Token-aligned diffing within the hunk
    let old_tokens = tokenize(old, old_start, old_end);
    let new_tokens = tokenize(new, new_start, new_end);

    if old_tokens.is_empty() && new_tokens.is_empty() {
        emit_range(result, old, new, old_start, old_end, new_start, new_end);
        return;
    }

    let token_ops = diff_slices(&old_tokens, &new_tokens);
    let mut old_cursor = old_start;
    let mut new_cursor = new_start;

    for op in token_ops {
        match op {
            LineDiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for i in 0..len {
                    let ot = &old_tokens[old_index + i];
                    let nt = &new_tokens[new_index + i];
                    emit_range(result, old, new, old_cursor, ot.start, new_cursor, nt.start);
                    result.push(ByteOp {
                        kind: DiffOpKind::Equal,
                        data: new.as_bytes()[nt.start..nt.end].to_vec(),
                    });
                    old_cursor = ot.end;
                    new_cursor = nt.end;
                }
            }
            LineDiffOp::Delete {
                old_index, old_len, ..
            } => {
                if old_len > 0 {
                    let start = old_tokens[old_index].start;
                    let end = old_tokens[old_index + old_len - 1].end;
                    emit_range(result, old, new, old_cursor, start, new_cursor, new_cursor);
                    result.push(ByteOp {
                        kind: DiffOpKind::Delete,
                        data: old.as_bytes()[start..end].to_vec(),
                    });
                    old_cursor = end;
                }
            }
            LineDiffOp::Insert {
                new_index, new_len, ..
            } => {
                if new_len > 0 {
                    let start = new_tokens[new_index].start;
                    let end = new_tokens[new_index + new_len - 1].end;
                    emit_range(result, old, new, old_cursor, old_cursor, new_cursor, start);
                    result.push(ByteOp {
                        kind: DiffOpKind::Insert,
                        data: new.as_bytes()[start..end].to_vec(),
                    });
                    new_cursor = end;
                }
            }
            LineDiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let os = old_tokens
                    .get(old_index)
                    .map(|t| t.start)
                    .unwrap_or(old_cursor);
                let ns = new_tokens
                    .get(new_index)
                    .map(|t| t.start)
                    .unwrap_or(new_cursor);
                emit_range(result, old, new, old_cursor, os, new_cursor, ns);

                if old_len > 0 {
                    let oe = old_tokens[old_index + old_len - 1].end;
                    result.push(ByteOp {
                        kind: DiffOpKind::Delete,
                        data: old.as_bytes()[os..oe].to_vec(),
                    });
                    old_cursor = oe;
                } else {
                    old_cursor = os;
                }
                if new_len > 0 {
                    let ne = new_tokens[new_index + new_len - 1].end;
                    result.push(ByteOp {
                        kind: DiffOpKind::Insert,
                        data: new.as_bytes()[ns..ne].to_vec(),
                    });
                    new_cursor = ne;
                } else {
                    new_cursor = ns;
                }
            }
        }
    }

    emit_range(result, old, new, old_cursor, old_end, new_cursor, new_end);
}

fn emit_range(
    result: &mut Vec<ByteOp>,
    old: &str,
    new: &str,
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
) {
    if old_start >= old_end && new_start >= new_end {
        return;
    }
    let old_slice = &old.as_bytes()[old_start..old_end];
    let new_slice = &new.as_bytes()[new_start..new_end];

    if !old_slice.is_empty() && !new_slice.is_empty() && old_slice == new_slice {
        result.push(ByteOp {
            kind: DiffOpKind::Equal,
            data: new_slice.to_vec(),
        });
        return;
    }
    if !old_slice.is_empty() {
        result.push(ByteOp {
            kind: DiffOpKind::Delete,
            data: old_slice.to_vec(),
        });
    }
    if !new_slice.is_empty() {
        result.push(ByteOp {
            kind: DiffOpKind::Insert,
            data: new_slice.to_vec(),
        });
    }
}

// ---------------------------------------------------------------------------
// Phase 2: Build catalogs
// ---------------------------------------------------------------------------

fn build_catalog(ops: &[ByteOp]) -> (Vec<Deletion>, Vec<Insertion>) {
    let mut deletions = Vec::new();
    let mut insertions = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;

    for op in ops {
        let len = op.data.len();
        match op.kind {
            DiffOpKind::Equal => {
                old_pos += len;
                new_pos += len;
            }
            DiffOpKind::Delete => {
                deletions.push(Deletion {
                    start: old_pos,
                    end: old_pos + len,
                });
                old_pos += len;
            }
            DiffOpKind::Insert => {
                insertions.push(Insertion {
                    start: new_pos,
                    end: new_pos + len,
                });
                new_pos += len;
            }
        }
    }

    (deletions, insertions)
}

// ---------------------------------------------------------------------------
// Phase 3: Move detection
// ---------------------------------------------------------------------------

const MOVE_THRESHOLD_LINES: usize = 3;

fn detect_moves(
    old_content: &str,
    new_content: &str,
    deletions: &[Deletion],
    insertions: &[Insertion],
) -> Vec<MoveMapping> {
    let old_lines = collect_lines(old_content);
    let new_lines = collect_lines(new_content);

    let mut deleted_entries: Vec<(usize, usize, String)> = Vec::new();
    for (di, del) in deletions.iter().enumerate() {
        for line in &old_lines {
            if line.start < del.end && line.end > del.start {
                let trimmed = old_content[line.start..line.end].trim().to_string();
                if !trimmed.is_empty() {
                    deleted_entries.push((di, line.number, trimmed));
                }
            }
        }
    }

    let mut inserted_entries: Vec<(usize, usize, String)> = Vec::new();
    for (ii, ins) in insertions.iter().enumerate() {
        for line in &new_lines {
            if line.start < ins.end && line.end > ins.start {
                let trimmed = new_content[line.start..line.end].trim().to_string();
                if !trimmed.is_empty() {
                    inserted_entries.push((ii, line.number, trimmed));
                }
            }
        }
    }

    if deleted_entries.is_empty() || inserted_entries.is_empty() {
        return Vec::new();
    }

    let del_groups = group_contiguous(&deleted_entries, MOVE_THRESHOLD_LINES);
    let ins_groups = group_contiguous(&inserted_entries, MOVE_THRESHOLD_LINES);

    if del_groups.is_empty() || ins_groups.is_empty() {
        return Vec::new();
    }

    // Hash-based lookup for deleted lines
    let mut del_lookup: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
    for (gi, group) in del_groups.iter().enumerate() {
        for (pos, entry) in group.iter().enumerate() {
            let h = hash_str(&entry.2);
            del_lookup.entry(h).or_default().push((gi, pos));
        }
    }

    let mut mappings = Vec::new();

    for ins_group in &ins_groups {
        let mut i = 0;
        while i < ins_group.len() {
            let h = hash_str(&ins_group[i].2);
            let mut matched = false;

            if let Some(candidates) = del_lookup.get(&h) {
                for &(dg_idx, dp) in candidates {
                    let dg = &del_groups[dg_idx];
                    if ins_group[i].2 != dg[dp].2 {
                        continue;
                    }

                    let mut match_len = 1;
                    while i + match_len < ins_group.len()
                        && dp + match_len < dg.len()
                        && ins_group[i + match_len].2 == dg[dp + match_len].2
                    {
                        match_len += 1;
                    }

                    if match_len >= MOVE_THRESHOLD_LINES {
                        let del_idx = dg[dp].0;
                        let ins_idx = ins_group[i].0;

                        let del = &deletions[del_idx];
                        let ins = &insertions[ins_idx];

                        let src_start = line_offset_in_range(&old_lines, dg[dp].1, del.start, true);
                        let src_end = line_offset_in_range(
                            &old_lines,
                            dg[dp + match_len - 1].1,
                            del.start,
                            false,
                        )
                        .min(del.end - del.start);
                        let tgt_start =
                            line_offset_in_range(&new_lines, ins_group[i].1, ins.start, true);
                        let tgt_end = line_offset_in_range(
                            &new_lines,
                            ins_group[i + match_len - 1].1,
                            ins.start,
                            false,
                        )
                        .min(ins.end - ins.start);

                        if src_start < src_end && tgt_start < tgt_end {
                            mappings.push(MoveMapping {
                                deletion_idx: del_idx,
                                insertion_idx: ins_idx,
                                source_range: (src_start, src_end),
                                target_range: (tgt_start, tgt_end),
                            });
                        }

                        i += match_len;
                        matched = true;
                        break;
                    }
                }
            }

            if !matched {
                i += 1;
            }
        }
    }

    mappings
}

struct LineMeta {
    number: usize,
    start: usize,
    end: usize,
}

fn collect_lines(content: &str) -> Vec<LineMeta> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut num = 1;
    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            lines.push(LineMeta {
                number: num,
                start,
                end: idx + 1,
            });
            start = idx + 1;
            num += 1;
        }
    }
    if start < content.len() {
        lines.push(LineMeta {
            number: num,
            start,
            end: content.len(),
        });
    }
    lines
}

fn group_contiguous<'a>(
    entries: &'a [(usize, usize, String)],
    threshold: usize,
) -> Vec<Vec<&'a (usize, usize, String)>> {
    let mut groups: Vec<Vec<&(usize, usize, String)>> = Vec::new();
    let mut current: Vec<&(usize, usize, String)> = Vec::new();
    let mut last_num: Option<usize> = None;

    for entry in entries {
        match last_num {
            Some(prev) if entry.1 == prev + 1 => current.push(entry),
            _ => {
                if current.len() >= threshold {
                    groups.push(current);
                }
                current = vec![entry];
            }
        }
        last_num = Some(entry.1);
    }
    if current.len() >= threshold {
        groups.push(current);
    }
    groups
}

/// Returns byte offset of a line relative to range_start.
/// If `is_start` is true, returns the start of the line; otherwise returns the end.
fn line_offset_in_range(
    lines: &[LineMeta],
    line_number: usize,
    range_start: usize,
    is_start: bool,
) -> usize {
    lines
        .iter()
        .find(|l| l.number == line_number)
        .map(|l| {
            if is_start {
                l.start.max(range_start) - range_start
            } else {
                l.end.saturating_sub(range_start)
            }
        })
        .unwrap_or(0)
}

fn hash_str(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// Phase 4: Transform attributions
// ---------------------------------------------------------------------------

fn transform_attributions(
    ops: &[ByteOp],
    old_attributions: &[Attribution],
    current_author: &str,
    ts: u128,
    insertions: &[Insertion],
    move_mappings: &[MoveMapping],
) -> Vec<Attribution> {
    let mut new_attrs: Vec<Attribution> = Vec::new();

    let mut deletion_to_moves: HashMap<usize, Vec<&MoveMapping>> = HashMap::new();
    let mut insertion_move_ranges: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();

    for m in move_mappings {
        deletion_to_moves.entry(m.deletion_idx).or_default().push(m);
        insertion_move_ranges
            .entry(m.insertion_idx)
            .or_default()
            .push(m.target_range);
    }

    let mut old_pos = 0usize;
    let mut new_pos = 0usize;
    let mut deletion_idx = 0usize;
    let mut insertion_idx = 0usize;
    let mut attr_cursor = 0usize;

    for op in ops {
        let len = op.data.len();
        match op.kind {
            DiffOpKind::Equal => {
                let old_range_start = old_pos;
                let old_range_end = old_pos + len;

                while attr_cursor < old_attributions.len()
                    && old_attributions[attr_cursor].end <= old_range_start
                {
                    attr_cursor += 1;
                }

                let mut idx = attr_cursor;
                while idx < old_attributions.len() {
                    let attr = &old_attributions[idx];
                    if attr.start >= old_range_end {
                        break;
                    }
                    let overlap_start = attr.start.max(old_range_start);
                    let overlap_end = attr.end.min(old_range_end);
                    if overlap_start < overlap_end {
                        let offset = overlap_start - old_range_start;
                        let overlap_len = overlap_end - overlap_start;
                        new_attrs.push(Attribution {
                            start: new_pos + offset,
                            end: new_pos + offset + overlap_len,
                            author_id: attr.author_id.clone(),
                            ts: attr.ts,
                        });
                    }
                    idx += 1;
                }

                old_pos += len;
                new_pos += len;
            }
            DiffOpKind::Delete => {
                let deletion_range_start = old_pos;

                if let Some(mappings) = deletion_to_moves.get(&deletion_idx) {
                    for m in mappings {
                        let source_start = deletion_range_start + m.source_range.0;
                        let source_end = deletion_range_start + m.source_range.1;
                        let target_start = insertions[m.insertion_idx].start + m.target_range.0;

                        let mut idx = attr_cursor;
                        while idx < old_attributions.len() {
                            let attr = &old_attributions[idx];
                            if attr.start >= source_end {
                                break;
                            }
                            let overlap_start = attr.start.max(source_start);
                            let overlap_end = attr.end.min(source_end);
                            if overlap_start < overlap_end {
                                let offset = overlap_start - source_start;
                                new_attrs.push(Attribution {
                                    start: target_start + offset,
                                    end: target_start + offset + (overlap_end - overlap_start),
                                    author_id: attr.author_id.clone(),
                                    ts: attr.ts,
                                });
                            }
                            idx += 1;
                        }
                    }
                }

                old_pos += len;
                deletion_idx += 1;
            }
            DiffOpKind::Insert => {
                if let Some(ranges) = insertion_move_ranges.remove(&insertion_idx) {
                    // Moved content: attribute gaps (non-moved portions) to current author
                    let mut covered: Vec<(usize, usize)> = ranges;
                    covered.sort_by_key(|r| r.0);
                    let mut merged: Vec<(usize, usize)> = Vec::new();
                    for (s, e) in covered {
                        if s >= e {
                            continue;
                        }
                        if let Some(last) = merged.last_mut() {
                            if s <= last.1 {
                                last.1 = last.1.max(e);
                            } else {
                                merged.push((s, e));
                            }
                        } else {
                            merged.push((s, e));
                        }
                    }

                    let mut cursor = 0usize;
                    for (s, e) in &merged {
                        let cs = (*s).min(len);
                        let ce = (*e).min(len);
                        if cursor < cs {
                            new_attrs.push(Attribution {
                                start: new_pos + cursor,
                                end: new_pos + cs,
                                author_id: current_author.to_string(),
                                ts,
                            });
                        }
                        cursor = cursor.max(ce);
                    }
                    if cursor < len {
                        new_attrs.push(Attribution {
                            start: new_pos + cursor,
                            end: new_pos + len,
                            author_id: current_author.to_string(),
                            ts,
                        });
                    }
                } else {
                    new_attrs.push(Attribution {
                        start: new_pos,
                        end: new_pos + len,
                        author_id: current_author.to_string(),
                        ts,
                    });
                }

                new_pos += len;
                insertion_idx += 1;
            }
        }
    }

    new_attrs
}

// ---------------------------------------------------------------------------
// Phase 5: Merge
// ---------------------------------------------------------------------------

fn merge_attributions(mut attrs: Vec<Attribution>) -> Vec<Attribution> {
    if attrs.is_empty() {
        return attrs;
    }

    attrs.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.end.cmp(&b.end))
            .then_with(|| a.author_id.cmp(&b.author_id))
            .then_with(|| a.ts.cmp(&b.ts))
    });
    attrs.dedup();

    let mut merged: Vec<Attribution> = Vec::with_capacity(attrs.len());
    for attr in attrs {
        if attr.start >= attr.end {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if last.author_id == attr.author_id && last.ts == attr.ts && attr.start <= last.end {
                last.end = last.end.max(attr.end);
                continue;
            }
        }
        merged.push(attr);
    }

    merged
}

/// For lines modified by deletion (but with no byte-level attribution in the new
/// content), attribute the entire line to the current author.
/// This handles cases where AI removes text from a line but the remaining Equal
/// bytes have no prior attribution.
fn attribute_deletion_touched_lines(
    merged: &[Attribution],
    diff_ops: &[ByteOp],
    new_content: &str,
    current_author: &str,
    ts: u128,
) -> Vec<Attribution> {
    // Find byte ranges in the NEW content that are on lines where a Delete occurred.
    // A Delete doesn't produce bytes in the new content directly, but it means
    // adjacent Equal bytes belong to a "modified" line.
    let line_ranges = compute_line_ranges(new_content);
    if line_ranges.is_empty() {
        return merged.to_vec();
    }

    // Track which new-content byte positions are adjacent to a deletion
    let mut deletion_adjacent_pos: Vec<usize> = Vec::new();
    let mut new_pos: usize = 0;
    let mut prev_was_delete = false;

    for op in diff_ops {
        match op.kind {
            DiffOpKind::Equal => {
                if prev_was_delete {
                    // The start of this Equal region is adjacent to a deletion
                    deletion_adjacent_pos.push(new_pos);
                }
                new_pos += op.data.len();
                prev_was_delete = false;
            }
            DiffOpKind::Delete => {
                // Mark the position just before this deletion as adjacent
                if new_pos > 0 {
                    deletion_adjacent_pos.push(new_pos - 1);
                }
                prev_was_delete = true;
            }
            DiffOpKind::Insert => {
                new_pos += op.data.len();
                prev_was_delete = false;
            }
        }
    }

    if deletion_adjacent_pos.is_empty() {
        return merged.to_vec();
    }

    // Find lines that contain deletion-adjacent positions
    let mut deletion_lines: Vec<usize> = Vec::new();
    for &pos in &deletion_adjacent_pos {
        for (idx, &(start, end)) in line_ranges.iter().enumerate() {
            if pos >= start && pos < end {
                deletion_lines.push(idx);
                break;
            }
        }
    }
    deletion_lines.sort_unstable();
    deletion_lines.dedup();

    // For each deletion-affected line, check if it has substantial attribution
    let mut result = merged.to_vec();
    for &line_idx in &deletion_lines {
        let (line_start, line_end) = line_ranges[line_idx];

        // Check if any existing attribution covers non-whitespace bytes on this line.
        // Whitespace-only coverage (e.g. a trailing newline from Insert) doesn't count
        // because it doesn't represent meaningful content attribution.
        let has_substantial_coverage = merged.iter().any(|attr| {
            if attr.end <= line_start || attr.start >= line_end {
                return false;
            }
            let overlap_start = attr.start.max(line_start);
            let overlap_end = attr.end.min(line_end);
            if overlap_start >= overlap_end {
                return false;
            }
            // Check if the overlapping bytes contain non-whitespace
            new_content[overlap_start..overlap_end]
                .chars()
                .any(|c| !c.is_whitespace())
        });

        if !has_substantial_coverage {
            // No substantial attribution for this line — attribute to current author
            result.push(Attribution {
                start: line_start,
                end: line_end,
                author_id: current_author.to_string(),
                ts,
            });
        }
    }

    // Re-merge after adding new attributions
    merge_attributions(result)
}

// ---------------------------------------------------------------------------
// Line attribution helpers
// ---------------------------------------------------------------------------

fn compute_line_ranges(content: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (idx, _) in content.match_indices('\n') {
        ranges.push((start, idx + 1));
        start = idx + 1;
    }
    if start < content.len() {
        ranges.push((start, content.len()));
    }
    ranges
}

fn find_dominant_author(
    line_start: usize,
    line_end: usize,
    is_blank: bool,
    sorted_attr_indices: &[usize],
    attributions: &[Attribution],
    content: &str,
) -> String {
    let mut best_substantial: Option<(&str, u128)> = None;
    let mut best_any: Option<(&str, u128)> = None;

    for &idx in sorted_attr_indices {
        let attr = &attributions[idx];
        if attr.start >= line_end {
            break;
        }
        if attr.end <= line_start {
            continue;
        }

        let overlap_start = attr.start.max(line_start);
        let overlap_end = attr.end.min(line_end);

        let has_overlap = overlap_start < overlap_end;
        let has_substance = if has_overlap {
            let safe_start = floor_char_boundary(content, overlap_start);
            let safe_end = ceil_char_boundary(content, overlap_end);
            if safe_start < safe_end {
                content[safe_start..safe_end]
                    .chars()
                    .any(|c| !c.is_whitespace())
            } else {
                false
            }
        } else {
            false
        };

        if has_substance || is_blank {
            match best_substantial {
                None => best_substantial = Some((&attr.author_id, attr.ts)),
                Some((_, best_ts)) if attr.ts > best_ts => {
                    best_substantial = Some((&attr.author_id, attr.ts));
                }
                _ => {}
            }
        }

        // Track any overlap (including whitespace-only) as a fallback
        if has_overlap {
            match best_any {
                None => best_any = Some((&attr.author_id, attr.ts)),
                Some((_, best_ts)) if attr.ts > best_ts => {
                    best_any = Some((&attr.author_id, attr.ts));
                }
                _ => {}
            }
        }
    }

    // Prefer substantial overlap (non-whitespace bytes covered).
    if let Some((author, _)) = best_substantial {
        return author.to_string();
    }
    // Fall back to whitespace-only overlap:
    // - For blank lines: always accept (whitespace IS the content)
    // - For non-blank lines with AI author: only accept if it's NOT solely a trailing
    //   newline. A trailing newline from AI appending lines is a formatting artifact.
    // - For non-blank lines with human (h_) author: always accept. Human trailing-newline
    //   coverage is meaningful because it indicates the human checkpoint covered this line.
    if let Some((author, _)) = best_any {
        if is_blank {
            return author.to_string();
        }
        let is_human_author = author.starts_with("h_") || author == "human";
        if is_human_author {
            return author.to_string();
        }
        // AI author: check if the whitespace-only coverage is solely a trailing newline.
        // If it covers any bytes OTHER than the last byte of the line (or the last
        // byte isn't a newline), it's meaningful (e.g. indentation changes).
        let is_trailing_newline_only = line_end > line_start
            && content.as_bytes().get(line_end - 1) == Some(&b'\n')
            && sorted_attr_indices.iter().all(|&idx| {
                let attr = &attributions[idx];
                if attr.start >= line_end || attr.end <= line_start {
                    return true; // Not on this line
                }
                let overlap_start = attr.start.max(line_start);
                let overlap_end = attr.end.min(line_end);
                // Only covers the trailing newline byte
                overlap_start >= line_end - 1 && overlap_end <= line_end
            });
        if !is_trailing_newline_only {
            return author.to_string();
        }
    }
    String::new()
}

fn floor_char_boundary(content: &str, idx: usize) -> usize {
    let mut i = idx.min(content.len());
    while i > 0 && !content.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(content: &str, idx: usize) -> usize {
    let mut i = idx.min(content.len());
    while i < content.len() && !content.is_char_boundary(i) {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Sorting utilities
// ---------------------------------------------------------------------------

fn is_sorted(attrs: &[Attribution]) -> bool {
    attrs.windows(2).all(|w| {
        (w[0].start, w[0].end, &w[0].author_id, w[0].ts)
            <= (w[1].start, w[1].end, &w[1].author_id, w[1].ts)
    })
}

fn sort_attributions(attrs: &[Attribution]) -> Vec<Attribution> {
    let mut sorted = attrs.to_vec();
    sorted.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.end.cmp(&b.end))
            .then_with(|| a.author_id.cmp(&b.author_id))
            .then_with(|| a.ts.cmp(&b.ts))
    });
    sorted
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Token {
    lexeme: String,
    start: usize,
    end: usize,
}

impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        self.lexeme == other.lexeme
    }
}
impl Eq for Token {}

impl Hash for Token {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.lexeme.hash(state);
    }
}

impl PartialOrd for Token {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Token {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.lexeme.cmp(&other.lexeme)
    }
}

fn tokenize(content: &str, start: usize, end: usize) -> Vec<Token> {
    if start >= end {
        return Vec::new();
    }

    let mut tokens = Vec::new();
    let mut i = start;

    while i < end {
        let ch = match content[i..].chars().next() {
            Some(c) => c,
            None => break,
        };
        let ch_len = ch.len_utf8();

        if ch.is_whitespace() {
            i += ch_len;
            continue;
        }

        // String literals
        if ch == '"' || ch == '\'' || ch == '`' {
            let token_start = i;
            let quote = ch;
            i += ch_len;
            let mut escaped = false;
            while i < end {
                let c = match content[i..].chars().next() {
                    Some(c) => c,
                    None => break,
                };
                let cl = c.len_utf8();
                i += cl;
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == quote {
                    break;
                }
            }
            tokens.push(Token {
                lexeme: content[token_start..i].to_string(),
                start: token_start,
                end: i,
            });
            continue;
        }

        // Identifiers
        if ch.is_alphabetic() || ch == '_' {
            let token_start = i;
            while i < end {
                let c = match content[i..].chars().next() {
                    Some(c) => c,
                    None => break,
                };
                if c.is_alphanumeric() || c == '_' {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(Token {
                lexeme: content[token_start..i].to_string(),
                start: token_start,
                end: i,
            });
            continue;
        }

        // Numbers
        if ch.is_ascii_digit() {
            let token_start = i;
            while i < end {
                let c = match content[i..].chars().next() {
                    Some(c) => c,
                    None => break,
                };
                if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(Token {
                lexeme: content[token_start..i].to_string(),
                start: token_start,
                end: i,
            });
            continue;
        }

        // Multi-char operators
        let peek = content[i + ch_len..end].chars().next();
        if let Some(op) = match_multi_char_op(ch, peek) {
            tokens.push(Token {
                lexeme: op.to_string(),
                start: i,
                end: i + op.len(),
            });
            i += op.len();
            continue;
        }

        // Single character token
        tokens.push(Token {
            lexeme: ch.to_string(),
            start: i,
            end: i + ch_len,
        });
        i += ch_len;
    }

    tokens
}

fn match_multi_char_op(ch: char, peek: Option<char>) -> Option<&'static str> {
    let p = peek?;
    match (ch, p) {
        ('=', '=') => Some("=="),
        ('!', '=') => Some("!="),
        ('<', '=') => Some("<="),
        ('>', '=') => Some(">="),
        ('&', '&') => Some("&&"),
        ('|', '|') => Some("||"),
        (':', ':') => Some("::"),
        ('-', '>') => Some("->"),
        ('=', '>') => Some("=>"),
        ('.', '.') => Some(".."),
        ('+', '+') => Some("++"),
        ('-', '-') => Some("--"),
        ('+', '=') => Some("+="),
        ('-', '=') => Some("-="),
        ('*', '=') => Some("*="),
        ('/', '=') => Some("/="),
        ('<', '<') => Some("<<"),
        ('>', '>') => Some(">>"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// imara-diff integration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum LineDiffOp {
    Equal {
        old_index: usize,
        new_index: usize,
        len: usize,
    },
    Delete {
        old_index: usize,
        old_len: usize,
        new_index: usize,
    },
    Insert {
        old_index: usize,
        new_index: usize,
        new_len: usize,
    },
    Replace {
        old_index: usize,
        old_len: usize,
        new_index: usize,
        new_len: usize,
    },
}

struct SliceSource<'a, T> {
    slice: &'a [T],
}

impl<'a, T: Clone + Hash + Eq> TokenSource for SliceSource<'a, T> {
    type Token = T;
    type Tokenizer = std::iter::Cloned<std::slice::Iter<'a, T>>;

    fn tokenize(&self) -> Self::Tokenizer {
        self.slice.iter().cloned()
    }

    fn estimate_tokens(&self) -> u32 {
        self.slice.len() as u32
    }
}

pub fn diff_slices<T: Hash + Eq + Clone>(old: &[T], new: &[T]) -> Vec<LineDiffOp> {
    let input = InternedInput::new(SliceSource { slice: old }, SliceSource { slice: new });
    let diff = Diff::compute(Algorithm::Myers, &input);
    hunks_to_ops(&diff, old.len())
}

#[allow(unused_assignments)]
fn hunks_to_ops(diff: &Diff, old_len: usize) -> Vec<LineDiffOp> {
    let mut ops = Vec::new();
    let mut old_idx = 0usize;
    let mut new_idx = 0usize;

    for hunk in diff.hunks() {
        let ho_start = hunk.before.start as usize;
        let ho_end = hunk.before.end as usize;
        let hn_start = hunk.after.start as usize;
        let hn_end = hunk.after.end as usize;

        if old_idx < ho_start {
            let eq_len = ho_start - old_idx;
            ops.push(LineDiffOp::Equal {
                old_index: old_idx,
                new_index: new_idx,
                len: eq_len,
            });
            new_idx += eq_len;
        }

        let old_hunk_len = ho_end - ho_start;
        let new_hunk_len = hn_end - hn_start;

        if old_hunk_len > 0 && new_hunk_len > 0 {
            ops.push(LineDiffOp::Replace {
                old_index: ho_start,
                old_len: old_hunk_len,
                new_index: hn_start,
                new_len: new_hunk_len,
            });
        } else if old_hunk_len > 0 {
            ops.push(LineDiffOp::Delete {
                old_index: ho_start,
                old_len: old_hunk_len,
                new_index: hn_start,
            });
        } else if new_hunk_len > 0 {
            ops.push(LineDiffOp::Insert {
                old_index: ho_start,
                new_index: hn_start,
                new_len: new_hunk_len,
            });
        }

        old_idx = ho_end;
        new_idx = hn_end;
    }

    if old_idx < old_len {
        let remaining = old_len - old_idx;
        ops.push(LineDiffOp::Equal {
            old_index: old_idx,
            new_index: new_idx,
            len: remaining,
        });
    }

    ops
}

// ---------------------------------------------------------------------------
// Line-level helpers for diff computation
// ---------------------------------------------------------------------------

fn line_slices(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (idx, _) in content.match_indices('\n') {
        lines.push(&content[start..idx + 1]);
        start = idx + 1;
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

fn line_byte_start(lines: &[&str], line_idx: usize) -> usize {
    lines[..line_idx].iter().map(|l| l.len()).sum()
}

fn line_byte_end(lines: &[&str], line_idx: usize, content_len: usize) -> usize {
    if line_idx >= lines.len() {
        content_len
    } else {
        lines[..line_idx].iter().map(|l| l.len()).sum()
    }
}

fn hunk_bounds(ops: &[LineDiffOp], for_old: bool) -> (usize, usize) {
    let mut start = usize::MAX;
    let mut end = 0usize;

    for op in ops {
        let (s, e) = match (op, for_old) {
            (LineDiffOp::Equal { old_index, len, .. }, true) => (*old_index, *old_index + *len),
            (LineDiffOp::Equal { new_index, len, .. }, false) => (*new_index, *new_index + *len),
            (
                LineDiffOp::Delete {
                    old_index, old_len, ..
                },
                true,
            ) => (*old_index, *old_index + *old_len),
            (LineDiffOp::Delete { new_index, .. }, false) => (*new_index, *new_index),
            (LineDiffOp::Insert { old_index, .. }, true) => (*old_index, *old_index),
            (
                LineDiffOp::Insert {
                    new_index, new_len, ..
                },
                false,
            ) => (*new_index, *new_index + *new_len),
            (
                LineDiffOp::Replace {
                    old_index, old_len, ..
                },
                true,
            ) => (*old_index, *old_index + *old_len),
            (
                LineDiffOp::Replace {
                    new_index, new_len, ..
                },
                false,
            ) => (*new_index, *new_index + *new_len),
        };
        start = start.min(s);
        end = end.max(e);
    }

    if start == usize::MAX {
        (0, 0)
    } else {
        (start, end)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(start: usize, end: usize, author: &str, ts: u128) -> Attribution {
        Attribution {
            start,
            end,
            author_id: author.to_string(),
            ts,
        }
    }

    fn assert_owned(attrs: &[Attribution], start: usize, end: usize, author: &str) {
        let owner = attrs
            .iter()
            .find(|a| a.start <= start && a.end >= end)
            .unwrap_or_else(|| panic!("no attribution covers {}..{}", start, end));
        assert_eq!(
            owner.author_id, author,
            "expected {} to own {}..{}, got {}",
            author, start, end, owner.author_id
        );
    }

    #[test]
    fn simple_insertion_attributed_to_new_author() {
        let old = "hello\n";
        let new = "hello\nworld\n";
        let prev = vec![attr(0, old.len(), "alice", 1)];

        let result = update_attributions(old, new, &prev, "bob", false);

        assert_owned(&result, 0, 5, "alice");
        let world_start = new.find("world").unwrap();
        assert_owned(&result, world_start, world_start + 5, "bob");
    }

    #[test]
    fn deletion_removes_attribution() {
        let old = "aaa\nbbb\nccc\n";
        let new = "aaa\nccc\n";
        let prev = vec![
            attr(0, 4, "alice", 1),
            attr(4, 8, "bob", 1),
            attr(8, 12, "alice", 1),
        ];

        let result = update_attributions(old, new, &prev, "carol", false);
        assert!(result.iter().all(|a| a.author_id != "bob"));
    }

    #[test]
    fn token_change_reattributes() {
        let old = "let x = 1;\n";
        let new = "let x = 2;\n";
        let prev = vec![attr(0, old.len(), "alice", 1)];

        let result = update_attributions(old, new, &prev, "bob", false);

        let pos = new.find('2').unwrap();
        assert_owned(&result, pos, pos + 1, "bob");
        assert_owned(&result, 0, 3, "alice");
    }

    #[test]
    fn line_attributions_basic() {
        let content = "line1\nline2\nline3\n";
        let attrs = vec![attr(0, 6, "alice", 1), attr(6, 18, "bob", 2)];

        let line_attrs = attributions_to_line_attributions(content, &attrs);

        assert_eq!(line_attrs.len(), 2);
        assert_eq!(line_attrs[0].start_line, 1);
        assert_eq!(line_attrs[0].end_line, 1);
        assert_eq!(line_attrs[0].author_id, "alice");
        assert_eq!(line_attrs[1].start_line, 2);
        assert_eq!(line_attrs[1].end_line, 3);
        assert_eq!(line_attrs[1].author_id, "bob");
    }

    #[test]
    fn whitespace_only_change_preserves_author() {
        let old = "fn test() {\n  do_stuff();\n}\n";
        let new = "fn test() {\n    do_stuff();\n}\n";
        let prev = vec![attr(0, old.len(), "alice", 1)];

        let result = update_attributions(old, new, &prev, "bob", false);

        let do_pos = new.find("do_stuff").unwrap();
        assert_owned(&result, do_pos, do_pos + 8, "alice");
    }

    #[test]
    fn unsorted_input_handled() {
        let old = "aaa\nbbb\n";
        let new = "aaa\nbbb\nccc\n";
        let prev = vec![attr(4, 8, "bob", 1), attr(0, 4, "alice", 1)];

        let result = update_attributions(old, new, &prev, "carol", false);
        assert_owned(&result, 0, 3, "alice");
        assert_owned(&result, 4, 7, "bob");
    }
}
