//! Attribution tracking through file changes
//!
//! This library maintains attribution ranges as files are edited, preserving
//! authorship information even through moves, edits, and whitespace changes.

use crate::authorship::move_detection::{DeletedLine, InsertedLine, detect_moves};
use crate::authorship::working_log::CheckpointKind;
use crate::error::GitAiError;
use diff_match_patch_rs::dmp::Diff;
use diff_match_patch_rs::traits::{Compat, Efficient};
use diff_match_patch_rs::{DiffMatchPatch, Ops};
use std::collections::HashMap;

/// Represents a single attribution range in the file.
/// Ranges can overlap (multiple authors can be attributed to the same text).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Attribution {
    /// Character position where this attribution starts (inclusive)
    pub start: usize,
    /// Character position where this attribution ends (exclusive)
    pub end: usize,
    /// Identifier for the author of this range
    pub author_id: String,
    /// Timestamp of the attribution (in milliseconds since epoch)
    pub ts: u128,
}

/// Represents attribution for a range of lines.
/// Both start_line and end_line are inclusive (1-indexed).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct LineAttribution {
    /// Line number where this attribution starts (inclusive, 1-indexed)
    pub start_line: u32,
    /// Line number where this attribution ends (inclusive, 1-indexed)
    pub end_line: u32,
    /// Identifier for the author of this range
    pub author_id: String,
    /// Author ID that was overwritten by this attribution (e.g., if Alice wrote this line originally, then Bob edited it, overwrote=Alice because her edit was writen over)
    #[serde(default)]
    pub overrode: Option<String>,
}

impl LineAttribution {
    pub fn new(
        start_line: u32,
        end_line: u32,
        author_id: String,
        overrode: Option<String>,
    ) -> Self {
        LineAttribution {
            start_line,
            end_line,
            author_id,
            overrode,
        }
    }

    /// Returns the number of lines this attribution covers
    #[allow(dead_code)]
    pub fn line_count(&self) -> u32 {
        if self.start_line > self.end_line {
            0
        } else {
            self.end_line - self.start_line + 1
        }
    }

    /// Checks if this line attribution is empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.start_line > self.end_line
    }

    /// Checks if this attribution overlaps with a given line range (inclusive)
    #[allow(dead_code)]
    pub fn overlaps(&self, start_line: u32, end_line: u32) -> bool {
        self.start_line <= end_line && self.end_line >= start_line
    }

    /// Returns the overlapping portion of this attribution with a given line range
    #[allow(dead_code)]
    pub fn intersection(&self, start_line: u32, end_line: u32) -> Option<(u32, u32)> {
        let overlap_start = self.start_line.max(start_line);
        let overlap_end = self.end_line.min(end_line);

        if overlap_start <= overlap_end {
            Some((overlap_start, overlap_end))
        } else {
            None
        }
    }
}

impl Attribution {
    pub fn new(start: usize, end: usize, author_id: String, ts: u128) -> Self {
        Attribution {
            start,
            end,
            author_id,
            ts,
        }
    }

    /// Returns the length of this attribution range
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Checks if this attribution is empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// Checks if this attribution overlaps with a given range
    pub fn overlaps(&self, start: usize, end: usize) -> bool {
        self.start < end && self.end > start
    }

    /// Returns the overlapping portion of this attribution with a given range
    pub fn intersection(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let overlap_start = self.start.max(start);
        let overlap_end = self.end.min(end);

        if overlap_start < overlap_end {
            Some((overlap_start, overlap_end))
        } else {
            None
        }
    }
}

/// Represents a deletion operation from the diff
#[derive(Debug, Clone)]
pub(crate) struct Deletion {
    /// Start position in old content
    pub(crate) start: usize,
    /// End position in old content
    pub(crate) end: usize,
    /// The deleted bytes (may not be valid UTF-8)
    #[allow(dead_code)]
    pub(crate) bytes: Vec<u8>,
}

/// Represents an insertion operation from the diff
#[derive(Debug, Clone)]
pub(crate) struct Insertion {
    /// Start position in new content
    pub(crate) start: usize,
    /// End position in new content
    pub(crate) end: usize,
    /// The inserted bytes (may not be valid UTF-8)
    #[allow(dead_code)]
    pub(crate) bytes: Vec<u8>,
}

/// Information about a detected move operation
#[derive(Debug, Clone)]
pub(crate) struct MoveMapping {
    /// The deletion that was moved
    pub(crate) deletion_idx: usize,
    /// The insertion where it was moved to
    pub(crate) insertion_idx: usize,
    /// Range within the deletion text that maps to the insertion (start, end) exclusive bounds
    pub(crate) source_range: (usize, usize),
    /// Range within the insertion text where the deletion text lands (start, end) exclusive bounds
    pub(crate) target_range: (usize, usize),
}

#[derive(Debug, Clone)]
struct LineMetadata {
    number: usize,
    start: usize,
    end: usize,
    text: String,
}

fn collect_line_metadata(content: &str) -> Vec<LineMetadata> {
    let mut metadata = Vec::new();
    let mut line_start = 0usize;
    let mut line_number = 1usize;

    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            let slice = &content[line_start..idx];
            let mut text = slice.to_string();
            if text.ends_with('\r') {
                text.pop();
            }
            metadata.push(LineMetadata {
                number: line_number,
                start: line_start,
                end: idx + 1,
                text,
            });
            line_start = idx + 1;
            line_number += 1;
        }
    }

    if line_start < content.len() {
        let slice = &content[line_start..content.len()];
        let mut text = slice.to_string();
        if text.ends_with('\r') {
            text.pop();
        }
        metadata.push(LineMetadata {
            number: line_number,
            start: line_start,
            end: content.len(),
            text,
        });
    }

    metadata
}

/// Configuration for the attribution tracker
pub struct AttributionConfig {
    move_lines_threshold: usize,
}

impl Default for AttributionConfig {
    fn default() -> Self {
        AttributionConfig {
            move_lines_threshold: 3,
        }
    }
}

/// Main attribution tracker
pub struct AttributionTracker {
    config: AttributionConfig,
    dmp: DiffMatchPatch,
}

impl AttributionTracker {
    /// Create a new attribution tracker with default configuration
    pub fn new() -> Self {
        AttributionTracker {
            config: AttributionConfig::default(),
            dmp: DiffMatchPatch::new(),
        }
    }

    /// Create a new attribution tracker with custom configuration
    #[allow(dead_code)]
    pub fn with_config(config: AttributionConfig) -> Self {
        AttributionTracker {
            config,
            dmp: DiffMatchPatch::new(),
        }
    }

    fn compute_diffs(
        &self,
        old_content: &str,
        new_content: &str,
    ) -> Result<Vec<Diff<u8>>, GitAiError> {
        let diffs = self
            .dmp
            .diff_main::<Efficient>(old_content, new_content)
            .map_err(|e| GitAiError::Generic(format!("Diff computation failed: {:?}", e)))?;

        if Self::diffs_are_char_aligned(&diffs, old_content, new_content) {
            return Ok(diffs);
        }

        let char_diffs = self
            .dmp
            .diff_main::<Compat>(old_content, new_content)
            .map_err(|e| GitAiError::Generic(format!("Diff computation failed: {:?}", e)))?;

        Ok(Self::convert_char_diffs_to_bytes(char_diffs))
    }

    fn convert_char_diffs_to_bytes(char_diffs: Vec<Diff<char>>) -> Vec<Diff<u8>> {
        let mut diffs = Vec::with_capacity(char_diffs.len());

        for diff in char_diffs {
            let op = diff.op();
            let data = diff.data();
            let mut bytes = Vec::with_capacity(data.len().saturating_mul(4));
            for ch in data {
                let mut buf = [0u8; 4];
                let encoded = ch.encode_utf8(&mut buf);
                bytes.extend_from_slice(encoded.as_bytes());
            }

            diffs.push(Diff::<u8>::new(op, &bytes));
        }

        diffs
    }

    fn diffs_are_char_aligned(diffs: &[Diff<u8>], old_content: &str, new_content: &str) -> bool {
        let mut old_pos = 0;
        let mut new_pos = 0;

        for diff in diffs {
            let len = diff.data().len();
            match diff.op() {
                Ops::Equal => {
                    if !Self::is_char_boundary_range(old_content, old_pos, old_pos + len)
                        || !Self::is_char_boundary_range(new_content, new_pos, new_pos + len)
                    {
                        return false;
                    }
                    old_pos += len;
                    new_pos += len;
                }
                Ops::Delete => {
                    if !Self::is_char_boundary_range(old_content, old_pos, old_pos + len) {
                        return false;
                    }
                    old_pos += len;
                }
                Ops::Insert => {
                    if !Self::is_char_boundary_range(new_content, new_pos, new_pos + len) {
                        return false;
                    }
                    new_pos += len;
                }
            }
        }

        true
    }

    fn is_char_boundary_range(s: &str, start: usize, end: usize) -> bool {
        if start > end || end > s.len() {
            return false;
        }
        s.is_char_boundary(start) && s.is_char_boundary(end)
    }

    /// Attribute all unattributed ranges to the given author
    pub fn attribute_unattributed_ranges(
        &self,
        content: &str,
        prev_attributions: &[Attribution],
        author: &str,
        ts: u128,
    ) -> Vec<Attribution> {
        let mut attributions = prev_attributions.to_vec();
        let mut unattributed_char_idxs = Vec::new();

        // Find all unattributed character positions
        for i in 0..content.len() {
            if !attributions.iter().any(|a| a.overlaps(i, i + 1)) {
                unattributed_char_idxs.push(i);
            }
        }

        // Sort the unattributed character indices by position
        unattributed_char_idxs.sort();

        // Group contiguous unattributed ranges
        let mut contiguous_ranges = Vec::new();
        if !unattributed_char_idxs.is_empty() {
            let mut start = unattributed_char_idxs[0];
            let mut end = start + 1;

            for i in 1..unattributed_char_idxs.len() {
                let current = unattributed_char_idxs[i];
                if current == end {
                    // Contiguous with previous range
                    end = current + 1;
                } else {
                    // Gap found, save current range and start new one
                    contiguous_ranges.push((start, end));
                    start = current;
                    end = current + 1;
                }
            }
            // Don't forget the last range
            contiguous_ranges.push((start, end));
        }

        // Create attributions for each contiguous unattributed range
        for (start, end) in contiguous_ranges {
            attributions.push(Attribution::new(start, end, author.to_string(), ts));
        }

        attributions
    }

    /// Update attributions from old content to new content
    ///
    /// # Arguments
    /// * `old_content` - The previous version of the file
    /// * `new_content` - The new version of the file
    /// * `old_attributions` - Attributions from the previous version
    /// * `current_author` - Author ID to use for new changes
    ///
    /// # Returns
    /// A vector of updated attributions for the new content
    pub fn update_attributions(
        &self,
        old_content: &str,
        new_content: &str,
        old_attributions: &[Attribution],
        current_author: &str,
        ts: u128,
    ) -> Result<Vec<Attribution>, GitAiError> {
        // Phase 1: Compute diff
        let diffs = self.compute_diffs(old_content, new_content)?;

        // Phase 2: Build deletion and insertion catalogs
        let (deletions, insertions) = self.build_diff_catalog(&diffs);

        // Phase 3: Detect move operations
        let move_mappings = self.detect_moves(old_content, new_content, &deletions, &insertions);

        // Phase 4: Transform attributions through the diff
        let new_attributions = self.transform_attributions(
            &diffs,
            old_attributions,
            current_author,
            &insertions,
            &move_mappings,
            ts,
        );

        // Phase 5: Merge and clean up
        Ok(self.merge_attributions(new_attributions))
    }

    /// Build catalogs of deletions and insertions from the diff
    fn build_diff_catalog(&self, diffs: &[Diff<u8>]) -> (Vec<Deletion>, Vec<Insertion>) {
        let mut deletions = Vec::new();
        let mut insertions = Vec::new();

        let mut old_pos = 0;
        let mut new_pos = 0;

        for diff in diffs {
            let op = diff.op();
            match op {
                Ops::Equal => {
                    let len = diff.data().len();
                    old_pos += len;
                    new_pos += len;
                }
                Ops::Delete => {
                    let bytes = diff.data();
                    let len = bytes.len();
                    deletions.push(Deletion {
                        start: old_pos,
                        end: old_pos + len,
                        bytes: bytes.to_vec(),
                    });
                    old_pos += len;
                }
                Ops::Insert => {
                    let bytes = diff.data();
                    let len = bytes.len();
                    insertions.push(Insertion {
                        start: new_pos,
                        end: new_pos + len,
                        bytes: bytes.to_vec(),
                    });
                    new_pos += len;
                }
            }
        }

        (deletions, insertions)
    }

    /// Detect move operations between deletions and insertions
    fn detect_moves(
        &self,
        old_content: &str,
        new_content: &str,
        deletions: &[Deletion],
        insertions: &[Insertion],
    ) -> Vec<MoveMapping> {
        let threshold = self.config.move_lines_threshold;
        if threshold == 0 || deletions.is_empty() || insertions.is_empty() {
            return Vec::new();
        }

        let old_lines = collect_line_metadata(old_content);
        let new_lines = collect_line_metadata(new_content);

        let old_line_map: HashMap<usize, LineMetadata> = old_lines
            .iter()
            .cloned()
            .map(|line| (line.number, line))
            .collect();
        let new_line_map: HashMap<usize, LineMetadata> = new_lines
            .iter()
            .cloned()
            .map(|line| (line.number, line))
            .collect();

        let mut inserted_lines: Vec<InsertedLine> = Vec::new();
        for (insertion_idx, insertion) in insertions.iter().enumerate() {
            for line in new_lines.iter() {
                if line.start < insertion.end && line.end > insertion.start {
                    inserted_lines.push(InsertedLine::new(
                        line.text.clone(),
                        line.number,
                        insertion_idx,
                    ));
                }
            }
        }

        let mut deleted_lines: Vec<DeletedLine> = Vec::new();
        for (deletion_idx, deletion) in deletions.iter().enumerate() {
            for line in old_lines.iter() {
                if line.start < deletion.end && line.end > deletion.start {
                    deleted_lines.push(DeletedLine::new(
                        line.text.clone(),
                        line.number,
                        deletion_idx,
                    ));
                }
            }
        }

        if inserted_lines.is_empty() || deleted_lines.is_empty() {
            return Vec::new();
        }

        let mut inserted_lines_slice = inserted_lines;
        let mut deleted_lines_slice = deleted_lines;
        let line_mappings = detect_moves(
            inserted_lines_slice.as_mut_slice(),
            deleted_lines_slice.as_mut_slice(),
            threshold,
        );

        let mut move_mappings = Vec::new();

        'mapping: for line_mapping in line_mappings {
            if line_mapping.deleted.is_empty() || line_mapping.inserted.is_empty() {
                continue;
            }
            if line_mapping.deleted.len() != line_mapping.inserted.len() {
                continue;
            }

            let deletion_idx = line_mapping.deleted[0].deletion_idx;
            if !line_mapping
                .deleted
                .iter()
                .all(|line| line.deletion_idx == deletion_idx)
            {
                continue;
            }

            let insertion_idx = line_mapping.inserted[0].insertion_idx;
            if !line_mapping
                .inserted
                .iter()
                .all(|line| line.insertion_idx == insertion_idx)
            {
                continue;
            }

            let deletion = match deletions.get(deletion_idx) {
                Some(value) => value,
                None => continue,
            };
            let insertion = match insertions.get(insertion_idx) {
                Some(value) => value,
                None => continue,
            };

            let mut source_start_opt: Option<usize> = None;
            let mut source_end_opt: Option<usize> = None;
            for deleted_line in &line_mapping.deleted {
                let meta = match old_line_map.get(&deleted_line.line_number) {
                    Some(meta) => meta,
                    None => continue 'mapping,
                };
                let start = meta.start.max(deletion.start);
                let end = meta.end.min(deletion.end);
                if start >= end {
                    continue 'mapping;
                }
                let rel_start = start - deletion.start;
                let rel_end = end - deletion.start;
                if source_start_opt.is_none() {
                    source_start_opt = Some(rel_start);
                }
                source_end_opt = Some(rel_end);
            }

            let mut target_start_opt: Option<usize> = None;
            let mut target_end_opt: Option<usize> = None;
            for inserted_line in &line_mapping.inserted {
                let meta = match new_line_map.get(&inserted_line.line_number) {
                    Some(meta) => meta,
                    None => continue 'mapping,
                };
                let start = meta.start.max(insertion.start);
                let end = meta.end.min(insertion.end);
                if start >= end {
                    continue 'mapping;
                }
                let rel_start = start - insertion.start;
                let rel_end = end - insertion.start;
                if target_start_opt.is_none() {
                    target_start_opt = Some(rel_start);
                }
                target_end_opt = Some(rel_end);
            }

            let (source_start, source_end) = match (source_start_opt, source_end_opt) {
                (Some(start), Some(end)) if start < end => (start, end),
                _ => continue,
            };
            let (target_start, target_end) = match (target_start_opt, target_end_opt) {
                (Some(start), Some(end)) if start < end => (start, end),
                _ => continue,
            };

            move_mappings.push(MoveMapping {
                deletion_idx,
                insertion_idx,
                source_range: (source_start, source_end),
                target_range: (target_start, target_end),
            });
        }

        move_mappings
    }

    /// Transform attributions through the diff
    fn transform_attributions(
        &self,
        diffs: &[Diff<u8>],
        old_attributions: &[Attribution],
        current_author: &str,
        insertions: &[Insertion],
        move_mappings: &[MoveMapping],
        ts: u128,
    ) -> Vec<Attribution> {
        let mut new_attributions = Vec::new();

        // Build lookup maps for moves
        let mut deletion_to_move: HashMap<usize, Vec<&MoveMapping>> = HashMap::new();
        let mut insertion_move_ranges: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();

        for mapping in move_mappings {
            let entry = deletion_to_move.entry(mapping.deletion_idx).or_default();
            if !entry.iter().any(|existing| {
                existing.source_range == mapping.source_range
                    && existing.target_range == mapping.target_range
            }) {
                entry.push(mapping);
            }
            insertion_move_ranges
                .entry(mapping.insertion_idx)
                .or_default()
                .push(mapping.target_range);
        }

        for mappings in deletion_to_move.values_mut() {
            mappings.sort_by_key(|m| m.source_range.0);
        }

        let mut old_pos = 0;
        let mut new_pos = 0;
        let mut deletion_idx = 0;
        let mut insertion_idx = 0;

        for diff in diffs {
            let op = diff.op();
            let len = diff.data().len();

            match op {
                Ops::Equal => {
                    // Unchanged text: transform attributions directly
                    let old_range = (old_pos, old_pos + len);
                    let new_range = (new_pos, new_pos + len);

                    for attr in old_attributions {
                        if let Some((overlap_start, overlap_end)) =
                            attr.intersection(old_range.0, old_range.1)
                        {
                            // Transform to new position
                            let offset_in_range = overlap_start - old_range.0;
                            let overlap_len = overlap_end - overlap_start;

                            new_attributions.push(Attribution::new(
                                new_range.0 + offset_in_range,
                                new_range.0 + offset_in_range + overlap_len,
                                attr.author_id.clone(),
                                attr.ts.clone(),
                            ));
                        }
                    }

                    old_pos += len;
                    new_pos += len;
                }
                Ops::Delete => {
                    let deletion_range = (old_pos, old_pos + len);

                    // Check if this deletion is part of a move
                    if let Some(mappings) = deletion_to_move.get(&deletion_idx) {
                        for mapping in mappings {
                            let insertion = &insertions[mapping.insertion_idx];
                            let source_start = deletion_range.0 + mapping.source_range.0;
                            let source_end = deletion_range.0 + mapping.source_range.1;

                            if source_start < source_end {
                                let target_start = insertion.start + mapping.target_range.0;

                                for attr in old_attributions {
                                    if let Some((overlap_start, overlap_end)) =
                                        attr.intersection(source_start, source_end)
                                    {
                                        let offset_in_source = overlap_start - source_start;
                                        let new_start = target_start + offset_in_source;
                                        let new_end = new_start + (overlap_end - overlap_start);

                                        if new_start < new_end {
                                            new_attributions.push(Attribution::new(
                                                new_start,
                                                new_end,
                                                attr.author_id.clone(),
                                                attr.ts,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // else: True deletion - attributions are lost

                    old_pos += len;
                    deletion_idx += 1;
                }
                Ops::Insert => {
                    // Check if this insertion is from a detected move
                    if let Some(ranges) = insertion_move_ranges.remove(&insertion_idx) {
                        let mut covered = ranges;
                        covered.sort_by_key(|r| r.0);

                        let mut merged: Vec<(usize, usize)> = Vec::new();
                        for (start, end) in covered {
                            if start >= end {
                                continue;
                            }

                            if let Some(last) = merged.last_mut() {
                                if start <= last.1 {
                                    last.1 = last.1.max(end);
                                } else {
                                    merged.push((start, end));
                                }
                            } else {
                                merged.push((start, end));
                            }
                        }

                        let mut cursor = 0usize;
                        for (start, end) in merged {
                            let clamped_start = start.min(len);
                            let clamped_end = end.min(len);

                            if cursor < clamped_start {
                                new_attributions.push(Attribution::new(
                                    new_pos + cursor,
                                    new_pos + clamped_start,
                                    current_author.to_string(),
                                    ts,
                                ));
                            }

                            cursor = cursor.max(clamped_end);
                        }

                        if cursor < len {
                            new_attributions.push(Attribution::new(
                                new_pos + cursor,
                                new_pos + len,
                                current_author.to_string(),
                                ts,
                            ));
                        }

                        new_pos += len;
                        insertion_idx += 1;
                        continue;
                    }

                    // Add attribution for this insertion
                    new_attributions.push(Attribution::new(
                        new_pos,
                        new_pos + len,
                        current_author.to_string(),
                        ts,
                    ));

                    new_pos += len;
                    insertion_idx += 1;
                }
            }
        }

        new_attributions
    }

    /// Merge and clean up attributions
    fn merge_attributions(&self, mut attributions: Vec<Attribution>) -> Vec<Attribution> {
        if attributions.is_empty() {
            return attributions;
        }

        // Sort by start position
        attributions.sort_by_key(|a| (a.start, a.end, a.author_id.clone()));

        // Remove exact duplicates
        attributions.dedup();

        attributions
    }
}

impl Default for AttributionTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper struct to track line boundaries in content
struct LineBoundaries {
    /// Maps line number (1-indexed) to (start_char, end_char) exclusive end
    line_ranges: Vec<(usize, usize)>,
}

impl LineBoundaries {
    fn new(content: &str) -> Self {
        let mut line_ranges = Vec::new();
        let mut start = 0;

        for (idx, _) in content.match_indices('\n') {
            // Line from start to idx (inclusive of newline)
            line_ranges.push((start, idx + 1));
            start = idx + 1;
        }

        // Handle last line if it doesn't end with newline
        if start < content.len() {
            line_ranges.push((start, content.len()));
        } else if start == content.len() && content.is_empty() {
            // Empty file - no lines
        } else if start == content.len() && !content.is_empty() {
            // File ends with newline, last line is already added
        }

        LineBoundaries { line_ranges }
    }

    fn line_count(&self) -> u32 {
        self.line_ranges.len() as u32
    }

    fn get_line_range(&self, line_num: u32) -> Option<(usize, usize)> {
        if line_num < 1 || line_num as usize > self.line_ranges.len() {
            None
        } else {
            Some(self.line_ranges[line_num as usize - 1])
        }
    }
}

/// Convert line-based attributions to character-based attributions.
///
/// # Arguments
/// * `line_attributions` - Line-based attributions to convert
/// * `content` - The file content to map line numbers to character positions
///
/// # Returns
/// A vector of character-based attributions covering the same ranges
pub fn line_attributions_to_attributions(
    line_attributions: &Vec<LineAttribution>,
    content: &str,
    ts: u128,
) -> Vec<Attribution> {
    if line_attributions.is_empty() || content.is_empty() {
        return Vec::new();
    }

    let boundaries = LineBoundaries::new(content);
    let mut result = Vec::new();

    for line_attr in line_attributions {
        // Get character ranges for start and end lines
        let start_range = boundaries.get_line_range(line_attr.start_line);
        let end_range = boundaries.get_line_range(line_attr.end_line);

        if let (Some((start_char, _)), Some((_, end_char))) = (start_range, end_range) {
            result.push(Attribution::new(
                start_char,
                end_char,
                line_attr.author_id.clone(),
                ts,
            ));
        }
    }

    result
}

/// Convert character-based attributions to line-based attributions.
/// For each line, selects the "dominant" author based on who contributed
/// the most non-whitespace characters to that line.
/// Finally, strip away all human-authored lines that aren't overrides.
///
/// # Arguments
/// * `attributions` - Character-based attributions
/// * `content` - The file content being attributed
///
/// # Returns
/// A vector of line attributions with consecutive lines by the same author merged
pub fn attributions_to_line_attributions(
    attributions: &Vec<Attribution>,
    content: &str,
) -> Vec<LineAttribution> {
    if content.is_empty() || attributions.is_empty() {
        return Vec::new();
    }

    let boundaries = LineBoundaries::new(content);
    let line_count = boundaries.line_count();

    if line_count == 0 {
        return Vec::new();
    }

    // For each line, determine the dominant author
    let mut line_authors: Vec<Option<(String, Option<String>)>> =
        Vec::with_capacity(line_count as usize);

    for line_num in 1..=line_count {
        let (author, overrode) =
            find_dominant_author_for_line(line_num, &boundaries, attributions, content);
        line_authors.push(Some((author, overrode)));
    }

    // Merge consecutive lines with the same author
    let mut merged_line_authors = merge_consecutive_line_attributions(line_authors);

    // Strip away all human lines (only AI lines need to be retained)
    merged_line_authors.retain(|line_attr| {
        line_attr.author_id != CheckpointKind::Human.to_str() || line_attr.overrode.is_some()
    });
    merged_line_authors
}

/// Find the dominant author for a specific line based on non-whitespace character count
fn find_dominant_author_for_line(
    line_num: u32,
    boundaries: &LineBoundaries,
    attributions: &Vec<Attribution>,
    full_content: &str,
) -> (String, Option<String>) {
    let (line_start, line_end) = boundaries.get_line_range(line_num).unwrap();
    let line_content = &full_content[line_start..line_end];
    let is_line_empty = line_content.is_empty() || line_content.chars().all(|c| c.is_whitespace());

    let mut candidate_attrs = Vec::new();
    for attribution in attributions {
        if !attribution.overlaps(line_start, line_end) {
            continue;
        }

        // Get the substring of the content on this line that is covered by the attribution
        let content_slice = &full_content[std::cmp::max(line_start, attribution.start)
            ..std::cmp::min(line_end, attribution.end)];
        let attr_non_whitespace_count =
            content_slice.chars().filter(|c| !c.is_whitespace()).count();
        if attr_non_whitespace_count > 0 || is_line_empty {
            candidate_attrs.push(attribution.clone());
        } else {
            // If the attribution is only whitespace, discard it
            continue;
        }
    }

    if candidate_attrs.is_empty() {
        return (CheckpointKind::Human.to_str(), None);
    }

    // Choose the author with the latest timestamp
    let latest_timestamp = candidate_attrs.iter().max_by_key(|a| a.ts).unwrap().ts;
    let latest_author = candidate_attrs
        .iter()
        .filter(|a| a.ts == latest_timestamp)
        .map(|a| a.author_id.clone())
        .collect::<Vec<String>>();
    let last_ai_edit = candidate_attrs
        .iter()
        .filter(|a| a.author_id != CheckpointKind::Human.to_str())
        .last();
    let last_human_edit = candidate_attrs
        .iter()
        .filter(|a| a.author_id == CheckpointKind::Human.to_str())
        .last();
    let overrode = match (last_ai_edit, last_human_edit) {
        (Some(ai), Some(h)) => {
            if h.ts > ai.ts {
                Some(ai.author_id.clone())
            } else {
                None
            }
        }
        _ => None,
    };
    return (latest_author[0].clone(), overrode);
}

/// Merge consecutive lines with the same author into LineAttribution ranges
fn merge_consecutive_line_attributions(
    line_authorship: Vec<Option<(String, Option<String>)>>,
) -> Vec<LineAttribution> {
    let mut result = Vec::new();
    let line_count = line_authorship.len();

    let mut current_authorship: Option<(String, Option<String>)> = None;
    let mut current_start: u32 = 0;

    for (idx, authorship) in line_authorship.into_iter().enumerate() {
        let line_num = (idx + 1) as u32;

        match (&current_authorship, authorship) {
            (None, None) => {
                // No attribution for this line, continue
            }
            (None, Some(new_author)) => {
                // Start a new line attribution
                current_authorship = Some(new_author);
                current_start = line_num;
            }
            (Some(_), None) => {
                // End current attribution
                if let Some(authorship) = current_authorship.take() {
                    result.push(LineAttribution::new(
                        current_start,
                        line_num - 1,
                        authorship.0,
                        authorship.1,
                    ));
                }
            }
            (Some(curr), Some(new_authorship)) => {
                if curr == &new_authorship {
                    // Continue current attribution
                } else {
                    // End current, start new
                    result.push(LineAttribution::new(
                        current_start,
                        line_num - 1,
                        curr.0.clone(),
                        curr.1.clone(),
                    ));
                    current_authorship = Some(new_authorship);
                    current_start = line_num;
                }
            }
        }
    }

    // Close final attribution if any
    if let Some(authorship) = current_authorship {
        result.push(LineAttribution::new(
            current_start,
            line_count as u32,
            authorship.0,
            authorship.1,
        ));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test timestamp constant for consistent testing
    const TEST_TS: u128 = 1234567890000;

    fn assert_range_owned_by(
        attributions: &[Attribution],
        range_start: usize,
        range_end: usize,
        expected_author: &str,
    ) {
        assert!(
            range_start < range_end,
            "Expected non-empty range, got {}..{}",
            range_start,
            range_end
        );

        assert!(
            attributions.iter().any(|attr| {
                attr.author_id == expected_author
                    && attr.start <= range_start
                    && attr.end >= range_end
            }),
            "Expected author {} to cover {}..{}, but attributions were {:?}",
            expected_author,
            range_start,
            range_end,
            attributions
        );

        for attr in attributions {
            if attr.overlaps(range_start, range_end) {
                assert_eq!(
                    attr.author_id, expected_author,
                    "Range {}..{} overlaps attribution {:?} owned by {}",
                    range_start, range_end, attr, attr.author_id
                );
            }
        }
    }

    fn module_move_old_content() -> &'static str {
        r#"module.exports =
  ({ enabled = true, logLevel, openAnalyzer, analyzerMode } = {}) =>
  (nextConfig = {}) => {
    if (!enabled) {
      return nextConfig
    }
    if (process.env.TURBOPACK) {
      console.warn(
        'The Next Bundle Analyzer is not compatible with Turbopack builds yet, no report will be generated.\n\n' +
          'To run this analysis pass the `--webpack` flag to `next build`'
      )
      return nextConfig
    }

    const extension = analyzerMode === 'json' ? '.json' : '.html'

    return Object.assign({}, nextConfig, {
      webpack(config, options) {
        const { BundleAnalyzerPlugin } = require('webpack-bundle-analyzer')
        config.plugins.push(
          new BundleAnalyzerPlugin({
            analyzerMode: analyzerMode || 'static',
            logLevel,
            openAnalyzer,
            reportFilename: !options.nextRuntime
              ? `./analyze/client${extension}`
              : `../${options.nextRuntime === 'nodejs' ? '../' : ''}analyze/${
                  options.nextRuntime
                }${extension}`,
          })
        )

        if (typeof nextConfig.webpack === 'function') {
          return nextConfig.webpack(config, options)
        }
        return config
      },
    })
  }"#
    }

    fn module_move_new_content() -> &'static str {
        r#"module.exports =
  ({ enabled = true, logLevel, openAnalyzer, analyzerMode } = {}) =>
  (nextConfig = {}) => {
    if (!enabled) {
      return nextConfig
    }
    if (process.env.TURBOPACK) {
      console.warn(
        'The Next Bundle Analyzer is not compatible with Turbopack builds yet, no report will be generated.\n\n' +
          'To run this analysis pass the `--webpack` flag to `next build`'
      )
      return nextConfig
    }

    const extension = analyzerMode === 'json' ? '.json' : '.html'

    return Object.assign({}, nextConfig, {
      webpack(config, options) {
        const { BundleAnalyzerPlugin } = require('webpack-bundle-analyzer')
        config.plugins.push(
          new BundleAnalyzerPlugin({
            analyzerMode: analyzerMode || 'static',
            logLevel,
            openAnalyzer,
            reportFilename: !options.nextRuntime
              ? `./analyze/client${extension}`
              : `../${options.nextRuntime === 'nodejs' ? '../' : ''}analyze/${
                  options.nextRuntime
                }${extension}`,
          })
        )


        return config
      },
    })
  }
  if (typeof nextConfig.webpack === 'function') {
    return nextConfig.webpack(config, options)
  }"#
    }

    #[test]
    fn test_simple_insertion() {
        let tracker = AttributionTracker::new();

        let old_content = "Hello world";
        let new_content = "Hello beautiful world";

        let old_attributions = vec![Attribution::new(0, 11, "Alice".to_string(), TEST_TS)];

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "Bob", TEST_TS)
            .unwrap();

        // Should have:
        // - "Hello " attributed to Alice
        // - "beautiful " attributed to Bob
        // - "world" attributed to Alice

        assert!(new_attributions.len() >= 3);

        // Check that "beautiful " is attributed to Bob
        let bob_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Bob")
            .collect();
        assert!(!bob_attrs.is_empty());
    }

    #[test]
    fn test_simple_deletion() {
        let tracker = AttributionTracker::new();

        let old_content = "Hello beautiful world";
        let new_content = "Hello world";

        let old_attributions = vec![
            Attribution::new(0, 6, "Alice".to_string(), TEST_TS),
            Attribution::new(6, 16, "Bob".to_string(), TEST_TS),
            Attribution::new(16, 21, "Alice".to_string(), TEST_TS),
        ];

        let new_attributions = tracker
            .update_attributions(
                old_content,
                new_content,
                &old_attributions,
                "Charlie",
                TEST_TS,
            )
            .unwrap();

        // Bob's attribution should be gone
        let bob_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Bob")
            .collect();
        assert!(bob_attrs.is_empty());

        // Alice's attributions should remain
        let alice_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Alice")
            .collect();
        assert!(!alice_attrs.is_empty());
    }

    #[test]
    fn test_cut_and_paste() {
        let tracker = AttributionTracker::new();

        // Original: function at the top
        let old_content =
            "fn helper() {\n  println!(\"helper\");\n}\n\nfn main() {\n  println!(\"main\");\n}";

        // New: function moved to bottom
        let new_content =
            "fn main() {\n  println!(\"main\");\n}\n\nfn helper() {\n  println!(\"helper\");\n}";

        // Attribute the helper function to Alice
        let old_attributions = vec![
            Attribution::new(0, 34, "Alice".to_string(), TEST_TS), // fn helper() { ... }
            Attribution::new(36, 70, "Bob".to_string(), TEST_TS),  // fn main() { ... }
        ];

        let new_attributions = tracker
            .update_attributions(
                old_content,
                new_content,
                &old_attributions,
                "Charlie",
                TEST_TS,
            )
            .unwrap();

        // Alice's attribution should move with the helper function
        // Bob's attribution should stay with the main function
        let alice_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Alice")
            .collect();

        let bob_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Bob")
            .collect();

        // Both authors should still have attributions
        assert!(
            !alice_attrs.is_empty(),
            "Alice's attribution should be preserved through move"
        );
        assert!(
            !bob_attrs.is_empty(),
            "Bob's attribution should be preserved"
        );
    }

    #[test]
    fn test_indentation_change() {
        let tracker = AttributionTracker::new();

        let old_content = "fn test() {\n  code();\n}";
        let new_content = "fn test() {\n    code();\n}"; // Changed from 2 to 4 space indent

        let old_attributions = vec![Attribution::new(0, 23, "Alice".to_string(), TEST_TS)];

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "Bob", TEST_TS)
            .unwrap();

        // Alice should still be attributed to most of the code
        let alice_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Alice")
            .collect();
        assert!(!alice_attrs.is_empty());

        // Bob should only be attributed to the extra spaces
        let bob_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Bob")
            .collect();

        // Bob gets attribution for the new whitespace
        assert!(!bob_attrs.is_empty());
    }

    #[test]
    fn test_overlapping_attributions() {
        let tracker = AttributionTracker::new();

        let old_content = "Hello world";
        let new_content = "Hello beautiful world";

        // Overlapping attributions: Alice owns 0-11, Bob owns 0-5
        let old_attributions = vec![
            Attribution::new(0, 11, "Alice".to_string(), TEST_TS),
            Attribution::new(0, 5, "Bob".to_string(), TEST_TS),
        ];

        let new_attributions = tracker
            .update_attributions(
                old_content,
                new_content,
                &old_attributions,
                "Charlie",
                TEST_TS,
            )
            .unwrap();

        // Both Alice and Bob should have overlapping attributions preserved
        let alice_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Alice")
            .collect();

        let bob_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Bob")
            .collect();

        assert!(!alice_attrs.is_empty());
        assert!(!bob_attrs.is_empty());
    }

    #[test]
    fn test_replacement() {
        let tracker = AttributionTracker::new();

        let old_content = "The quick brown fox";
        let new_content = "The slow brown fox";

        let old_attributions = vec![Attribution::new(0, 19, "Alice".to_string(), TEST_TS)];

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "Bob", TEST_TS)
            .unwrap();

        // "The " should be Alice
        // "slow" should be Bob
        // " brown fox" should be Alice

        let bob_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Bob")
            .collect();
        assert!(
            !bob_attrs.is_empty(),
            "Bob should be attributed for the replacement"
        );

        let alice_attrs: Vec<_> = new_attributions
            .iter()
            .filter(|a| a.author_id == "Alice")
            .collect();
        assert!(
            !alice_attrs.is_empty(),
            "Alice should retain attribution for unchanged parts"
        );
    }

    #[test]
    fn test_empty_file() {
        let tracker = AttributionTracker::new();

        let old_content = "";
        let new_content = "Hello world";

        let old_attributions = vec![];

        let new_attributions = tracker
            .update_attributions(
                old_content,
                new_content,
                &old_attributions,
                "Alice",
                TEST_TS,
            )
            .unwrap();

        assert_eq!(new_attributions.len(), 1);
        assert_eq!(new_attributions[0].author_id, "Alice");
        assert_eq!(new_attributions[0].start, 0);
        assert_eq!(new_attributions[0].end, 11);
    }

    #[test]
    fn test_no_changes() {
        let tracker = AttributionTracker::new();

        let old_content = "Hello world";
        let new_content = "Hello world";

        let old_attributions = vec![Attribution::new(0, 11, "Alice".to_string(), TEST_TS)];

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "Bob", TEST_TS)
            .unwrap();

        assert_eq!(new_attributions.len(), 1);
        assert_eq!(new_attributions[0].author_id, "Alice");
        assert_eq!(new_attributions[0].start, 0);
        assert_eq!(new_attributions[0].end, 11);
    }

    #[test]
    fn test_attribution_overlap() {
        let attr = Attribution::new(10, 20, "Alice".to_string(), TEST_TS);

        assert!(attr.overlaps(15, 25));
        assert!(attr.overlaps(5, 15));
        assert!(attr.overlaps(5, 25));
        assert!(attr.overlaps(12, 18));
        assert!(!attr.overlaps(0, 10));
        assert!(!attr.overlaps(20, 30));
    }

    #[test]
    fn test_attribution_intersection() {
        let attr = Attribution::new(10, 20, "Alice".to_string(), TEST_TS);

        assert_eq!(attr.intersection(15, 25), Some((15, 20)));
        assert_eq!(attr.intersection(5, 15), Some((10, 15)));
        assert_eq!(attr.intersection(5, 25), Some((10, 20)));
        assert_eq!(attr.intersection(12, 18), Some((12, 18)));
        assert_eq!(attr.intersection(0, 10), None);
        assert_eq!(attr.intersection(20, 30), None);
    }

    #[test]
    fn test_multiline_text_with_move() {
        let tracker = AttributionTracker::new();

        let old_content = r#"// Header
fn foo() {
    bar();
}

fn main() {
    foo();
}"#;

        let new_content = r#"// Header
fn main() {
    foo();
}

fn foo() {
    bar();
}"#;

        // Attribute different functions to different authors
        let old_attributions = vec![
            Attribution::new(0, 10, "Alice".to_string(), TEST_TS), // // Header
            Attribution::new(10, 34, "Bob".to_string(), TEST_TS),  // fn foo() { bar(); }
            Attribution::new(35, 63, "Charlie".to_string(), TEST_TS), // fn main() { foo(); }
        ];

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "Dave", TEST_TS)
            .unwrap();

        // All three authors should still have attributions
        let alice_count = new_attributions
            .iter()
            .filter(|a| a.author_id == "Alice")
            .count();
        let bob_count = new_attributions
            .iter()
            .filter(|a| a.author_id == "Bob")
            .count();
        let charlie_count = new_attributions
            .iter()
            .filter(|a| a.author_id == "Charlie")
            .count();

        assert!(
            alice_count > 0,
            "Alice's header attribution should be preserved"
        );
        assert!(bob_count > 0, "Bob's foo function attribution should move");
        assert!(
            charlie_count > 0,
            "Charlie's main function attribution should move"
        );
    }

    #[test]
    fn test_newline_insertion() {
        let tracker = AttributionTracker::new();

        // A creates two lines
        let v1_content = "console.log(\"A- HELLO\")\n";
        let v1_attributions = vec![Attribution::new(0, 24, "A".to_string(), TEST_TS)];

        // B adds a line
        let v2_content = "console.log(\"A- HELLO\")\nconsole.log(\"B- HELLO\")\n";
        let v2_attributions = tracker
            .update_attributions(v1_content, v2_content, &v1_attributions, "B", TEST_TS)
            .unwrap();

        // A adds three empty lines between B's line and the next line
        let v3_content = "console.log(\"A- HELLO\")\nconsole.log(\"B- HELLO\")\n\n\n\n";
        let v3_attributions = tracker
            .update_attributions(v2_content, v3_content, &v2_attributions, "A", TEST_TS)
            .unwrap();

        // C adds a line
        let v4_content =
            "console.log(\"A- HELLO\")\nconsole.log(\"B- HELLO\")\n\n\n\nconsole.log(\"C- HELLO\")";
        let v4_attributions = tracker
            .update_attributions(v3_content, v4_content, &v3_attributions, "C", TEST_TS)
            .unwrap();

        // Verify attributions
        // Line 1 (0-23) + newline (23) = A
        // Line 2 (24-47) + newline (47) = B
        // Empty line newline (48) = A
        // Empty line newline (49) = A
        // Empty line newline (50) = A
        // Line 6 (51-73) = C

        let a_attrs: Vec<_> = v4_attributions
            .iter()
            .filter(|a| a.author_id == "A")
            .collect();
        let b_attrs: Vec<_> = v4_attributions
            .iter()
            .filter(|a| a.author_id == "B")
            .collect();
        let c_attrs: Vec<_> = v4_attributions
            .iter()
            .filter(|a| a.author_id == "C")
            .collect();

        // A should have the first line + its newline, and the three empty line newlines
        // That's char 0-24 and chars 48-51 (3 newlines)
        let a_total: usize = a_attrs.iter().map(|a| a.len()).sum();
        assert_eq!(
            a_total,
            24 + 3,
            "A should have 24 chars from first line + 3 newlines"
        );

        // B should have the second line + its newline = 24 chars
        let b_total: usize = b_attrs.iter().map(|a| a.len()).sum();
        assert_eq!(b_total, 24, "B should have 24 chars");

        // C should have the last line (no trailing newline) = 23 chars
        let c_total: usize = c_attrs.iter().map(|a| a.len()).sum();
        assert_eq!(c_total, 23, "C should have 23 chars");

        // Check that the three newlines (chars 48-51) are all attributed to A
        for pos in 48..51 {
            let attributed_to_a = a_attrs.iter().any(|a| a.start <= pos && a.end > pos);
            assert!(
                attributed_to_a,
                "Character at position {} should be attributed to A",
                pos
            );
        }

        // Ensure C doesn't have any attribution in the 47-51 range (the newlines)
        for attr in &c_attrs {
            assert!(
                attr.start >= 51 || attr.end <= 47,
                "C should not have attribution in the newline range 47-51, but has {:?}",
                attr
            );
        }
    }

    #[test]
    fn test_update_attributions_handles_utf8_characters() {
        let tracker = AttributionTracker::new();

        let old_content = "😀\n";
        let new_content = "😀\n🙂\n";

        let old_attributions = vec![Attribution::new(
            0,
            old_content.len(),
            "Alice".to_string(),
            TEST_TS,
        )];

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "Bob", TEST_TS)
            .unwrap();

        let alice_attr = new_attributions
            .iter()
            .find(|a| a.author_id == "Alice")
            .expect("Alice attribution missing");

        assert_eq!(alice_attr.start, 0, "Alice start should remain at byte 0");
        assert_eq!(
            alice_attr.end,
            old_content.len(),
            "Alice end should match original UTF-8 byte length"
        );

        let line_attrs = attributions_to_line_attributions(&new_attributions, new_content);

        assert_eq!(line_attrs.len(), 2, "Each line should keep its author");
        assert_eq!(line_attrs[0].author_id, "Alice");
        assert_eq!(line_attrs[1].author_id, "Bob");
    }

    // ========== Line to Character Attribution Conversion Tests ==========

    #[test]
    fn test_line_to_char_attribution_single_range() {
        let content = "line 1\nline 2\nline 3\n";
        let line_attrs = vec![LineAttribution::new(1, 3, "Alice".to_string(), None)];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        assert_eq!(char_attrs.len(), 1);
        assert_eq!(char_attrs[0].start, 0);
        assert_eq!(char_attrs[0].end, 21); // entire content
        assert_eq!(char_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_to_char_attribution_multiple_ranges() {
        let content = "line 1\nline 2\nline 3\nline 4\n";
        let line_attrs = vec![
            LineAttribution::new(1, 2, "Alice".to_string(), None),
            LineAttribution::new(3, 4, "Bob".to_string(), None),
        ];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        assert_eq!(char_attrs.len(), 2);
        assert_eq!(char_attrs[0].start, 0);
        assert_eq!(char_attrs[0].end, 14); // lines 1-2
        assert_eq!(char_attrs[0].author_id, "Alice");
        assert_eq!(char_attrs[1].start, 14);
        assert_eq!(char_attrs[1].end, 28); // lines 3-4
        assert_eq!(char_attrs[1].author_id, "Bob");
    }

    #[test]
    fn test_line_to_char_attribution_single_line() {
        let content = "line 1\nline 2\nline 3\n";
        let line_attrs = vec![LineAttribution::new(2, 2, "Bob".to_string(), None)];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        assert_eq!(char_attrs.len(), 1);
        assert_eq!(char_attrs[0].start, 7);
        assert_eq!(char_attrs[0].end, 14); // just line 2
        assert_eq!(char_attrs[0].author_id, "Bob");
    }

    #[test]
    fn test_line_to_char_attribution_no_trailing_newline() {
        let content = "line 1\nline 2";
        let line_attrs = vec![LineAttribution::new(1, 2, "Alice".to_string(), None)];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        assert_eq!(char_attrs.len(), 1);
        assert_eq!(char_attrs[0].start, 0);
        assert_eq!(char_attrs[0].end, 13); // entire content
        assert_eq!(char_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_to_char_attribution_empty_input() {
        let content = "line 1\nline 2\n";
        let line_attrs = vec![];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        assert_eq!(char_attrs.len(), 0);
    }

    #[test]
    fn test_update_attributions_replacing_multibyte_emoji_succeeds() {
        let tracker = AttributionTracker::new();

        let old_content = "❌";
        let new_content = "✅";

        let old_attributions = vec![Attribution::new(
            0,
            old_content.len(),
            "Alice".to_string(),
            TEST_TS,
        )];

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "Bob", TEST_TS)
            .unwrap();

        assert!(
            new_attributions
                .iter()
                .all(|attr| attr.author_id != "Alice"),
            "Old author attribution should be removed after replacement: {:?}",
            new_attributions
        );

        let bob_attr = new_attributions
            .iter()
            .find(|attr| attr.author_id == "Bob")
            .expect("New content should be attributed to Bob");

        assert_eq!(bob_attr.start, 0, "New attribution should start at 0");
        assert_eq!(
            bob_attr.end,
            new_content.len(),
            "New attribution should cover entire replacement"
        );
    }

    #[test]
    fn test_update_attributions_plain_then_emoji_then_plain_suffix() {
        let tracker = AttributionTracker::new();

        let alice = "Alice";
        let bob = "Bob";
        let carol = "Carol";
        let dave = "Dave";

        let initial = "Start middle end";
        let mut attributions = vec![Attribution::new(
            0,
            initial.len(),
            alice.to_string(),
            TEST_TS,
        )];

        let plain_suffix = "Start changed middle end";
        attributions = tracker
            .update_attributions(initial, plain_suffix, &attributions, bob, TEST_TS + 1)
            .unwrap();
        let inserted_slice = "changed ";
        let inserted_start = plain_suffix
            .find(inserted_slice)
            .expect("expected inserted slice to exist");
        let inserted_end = inserted_start + inserted_slice.len();
        assert_range_owned_by(&attributions, inserted_start, inserted_end, bob);

        let with_emoji = "Start changed ✅ middle end";
        attributions = tracker
            .update_attributions(plain_suffix, with_emoji, &attributions, carol, TEST_TS + 2)
            .unwrap();
        let emoji_slice = "✅ ";
        let emoji_start = with_emoji
            .find(emoji_slice)
            .expect("expected emoji slice to exist");
        let emoji_end = emoji_start + emoji_slice.len();
        assert_range_owned_by(&attributions, emoji_start, emoji_end, carol);

        let final_content = "Start changed ✅ middle end updated";
        attributions = tracker
            .update_attributions(with_emoji, final_content, &attributions, dave, TEST_TS + 3)
            .unwrap();
        let suffix_slice = " updated";
        let suffix_start = final_content
            .rfind(suffix_slice)
            .expect("expected suffix slice to exist");
        let suffix_end = suffix_start + suffix_slice.len();

        assert_range_owned_by(&attributions, suffix_start, suffix_end, dave);
        assert_range_owned_by(&attributions, emoji_start, emoji_end, carol);
        assert_range_owned_by(&attributions, inserted_start, inserted_end, bob);

        let unique_authors: std::collections::HashSet<&str> = attributions
            .iter()
            .map(|attr| attr.author_id.as_str())
            .collect();
        assert!(unique_authors.contains(alice));
        assert!(unique_authors.contains(bob));
        assert!(unique_authors.contains(carol));
        assert!(unique_authors.contains(dave));
    }

    #[test]
    fn test_update_attributions_plain_then_emoji_then_plain_prefix() {
        let tracker = AttributionTracker::new();

        let alice = "Alice";
        let bob = "Bob";
        let carol = "Carol";
        let dave = "Dave";

        let initial = "Alpha middle tail";
        let mut attributions = vec![Attribution::new(
            0,
            initial.len(),
            alice.to_string(),
            TEST_TS,
        )];

        let with_suffix = "Alpha middle tail!";
        attributions = tracker
            .update_attributions(initial, with_suffix, &attributions, bob, TEST_TS + 1)
            .unwrap();
        let exclamation_start = with_suffix.len() - 1;
        let exclamation_end = with_suffix.len();
        assert_range_owned_by(&attributions, exclamation_start, exclamation_end, bob);

        let with_emoji = "Alpha middle ✅ tail!";
        attributions = tracker
            .update_attributions(with_suffix, with_emoji, &attributions, carol, TEST_TS + 2)
            .unwrap();
        let emoji_slice = "✅ ";
        let emoji_start = with_emoji
            .find(emoji_slice)
            .expect("expected emoji slice to exist");
        let emoji_end = emoji_start + emoji_slice.len();
        assert_range_owned_by(&attributions, emoji_start, emoji_end, carol);

        let final_content = "Updated Alpha middle ✅ tail!";
        attributions = tracker
            .update_attributions(with_emoji, final_content, &attributions, dave, TEST_TS + 3)
            .unwrap();
        let prefix_slice = "Updated ";
        let prefix_start = final_content
            .find(prefix_slice)
            .expect("expected prefix slice to exist");
        let prefix_end = prefix_start + prefix_slice.len();
        let final_emoji_start = final_content
            .find(emoji_slice)
            .expect("expected emoji slice to exist");
        let final_emoji_end = final_emoji_start + emoji_slice.len();
        let final_exclamation_start = final_content.len() - 1;
        let final_exclamation_end = final_content.len();

        assert_range_owned_by(&attributions, prefix_start, prefix_end, dave);
        assert_range_owned_by(&attributions, final_emoji_start, final_emoji_end, carol);
        assert_range_owned_by(
            &attributions,
            final_exclamation_start,
            final_exclamation_end,
            bob,
        );

        let unique_authors: std::collections::HashSet<&str> = attributions
            .iter()
            .map(|attr| attr.author_id.as_str())
            .collect();
        assert!(unique_authors.contains(alice));
        assert!(unique_authors.contains(bob));
        assert!(unique_authors.contains(carol));
        assert!(unique_authors.contains(dave));
    }

    #[test]
    fn test_update_attributions_mixed_language_sequence() {
        let tracker = AttributionTracker::new();

        let alice = "Alice";
        let bob = "Bob";
        let carol = "Carol";
        let dave = "Dave";

        let initial = "English: Hello | 日本語: こんにちは | العربية: مرحبا";
        let mut attributions = vec![Attribution::new(
            0,
            initial.len(),
            alice.to_string(),
            TEST_TS,
        )];

        let find_range = |haystack: &str, needle: &str| -> (usize, usize) {
            let start = haystack
                .find(needle)
                .unwrap_or_else(|| panic!("`{needle}` not found in `{haystack}`"));
            (start, start + needle.len())
        };

        let step_one = "English: Hello y hola | 日本語: こんにちは | العربية: مرحبا";
        attributions = tracker
            .update_attributions(initial, step_one, &attributions, bob, TEST_TS + 1)
            .unwrap();
        let (hola_start, hola_end) = find_range(step_one, "y hola");
        assert_range_owned_by(&attributions, hola_start, hola_end, bob);

        let step_two =
            "English: Hello y hola | 日本語: こんにちは🌸 と 中文: 你好 | العربية: مرحبا";
        attributions = tracker
            .update_attributions(step_one, step_two, &attributions, carol, TEST_TS + 2)
            .unwrap();
        let (japanese_base_start, japanese_base_end) = find_range(step_two, "日本語: こんにちは");
        let (japanese_extension_start, japanese_extension_end) =
            find_range(step_two, "🌸 と 中文: 你好");
        assert_range_owned_by(&attributions, japanese_base_start, japanese_base_end, alice);
        assert_range_owned_by(
            &attributions,
            japanese_extension_start,
            japanese_extension_end,
            carol,
        );
        assert_range_owned_by(&attributions, hola_start, hola_end, bob);

        let final_content = "Prelude ✨ | English: Hello y hola | 日本語: こんにちは🌸 と 中文: 你好 | العربية: مرحبا وسهلاً | Coda ✅";
        attributions = tracker
            .update_attributions(step_two, final_content, &attributions, dave, TEST_TS + 3)
            .unwrap();

        let (prefix_start, prefix_end) = find_range(final_content, "Prelude ✨ | ");
        let (suffix_start, suffix_end) = find_range(final_content, " | Coda ✅");
        let (arabic_extension_start, arabic_extension_end) = find_range(final_content, " وسهلاً");
        let (final_hola_start, final_hola_end) = find_range(final_content, "y hola");
        let (final_japanese_base_start, final_japanese_base_end) =
            find_range(final_content, "日本語: こんにちは");
        let (final_japanese_extension_start, final_japanese_extension_end) =
            find_range(final_content, "🌸 と 中文: 你好");
        let (english_core_start, english_core_end) = find_range(final_content, "English: Hello");
        let (arabic_core_start, arabic_core_end) = find_range(final_content, "العربية: مرحبا");

        assert_range_owned_by(&attributions, prefix_start, prefix_end, dave);
        assert_range_owned_by(&attributions, suffix_start, suffix_end, dave);
        assert_range_owned_by(
            &attributions,
            arabic_extension_start,
            arabic_extension_end,
            dave,
        );
        assert_range_owned_by(
            &attributions,
            final_japanese_base_start,
            final_japanese_base_end,
            alice,
        );
        assert_range_owned_by(
            &attributions,
            final_japanese_extension_start,
            final_japanese_extension_end,
            carol,
        );
        assert_range_owned_by(&attributions, final_hola_start, final_hola_end, bob);
        assert_range_owned_by(&attributions, english_core_start, english_core_end, alice);
        assert_range_owned_by(&attributions, arabic_core_start, arabic_core_end, alice);

        let unique_authors: std::collections::HashSet<&str> = attributions
            .iter()
            .map(|attr| attr.author_id.as_str())
            .collect();
        assert_eq!(
            unique_authors.len(),
            4,
            "Expected exactly four unique authors, got {:?}",
            unique_authors
        );
        assert!(unique_authors.contains(alice));
        assert!(unique_authors.contains(bob));
        assert!(unique_authors.contains(carol));
        assert!(unique_authors.contains(dave));
    }

    #[test]
    fn test_line_to_char_attribution_empty_content() {
        let content = "";
        let line_attrs = vec![LineAttribution::new(1, 1, "Alice".to_string(), None)];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        assert_eq!(char_attrs.len(), 0);
    }

    #[test]
    fn test_line_to_char_attribution_invalid_line_numbers() {
        let content = "line 1\nline 2\n";
        // Line 5 doesn't exist (only 2 lines)
        let line_attrs = vec![LineAttribution::new(5, 10, "Alice".to_string(), None)];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        // Should skip invalid line ranges
        assert_eq!(char_attrs.len(), 0);
    }

    #[test]
    fn test_line_to_char_attribution_realistic_code() {
        let content = r#"fn main() {
    println!("hello");
}

fn test() {
    assert!(true);
}
"#;
        let line_attrs = vec![
            LineAttribution::new(1, 3, "Alice".to_string(), None),
            LineAttribution::new(5, 7, "Bob".to_string(), None),
        ];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        assert_eq!(char_attrs.len(), 2);
        assert_eq!(char_attrs[0].author_id, "Alice");
        assert_eq!(char_attrs[1].author_id, "Bob");

        // Verify the character ranges map to correct content
        assert!(content[char_attrs[0].start..char_attrs[0].end].contains("fn main()"));
        assert!(content[char_attrs[1].start..char_attrs[1].end].contains("fn test()"));
    }

    #[test]
    fn test_line_to_char_attribution_preserves_author_order() {
        let content = "a\nb\nc\nd\ne\n";
        let line_attrs = vec![
            LineAttribution::new(1, 1, "Alice".to_string(), None),
            LineAttribution::new(2, 2, "Bob".to_string(), None),
            LineAttribution::new(3, 3, "Charlie".to_string(), None),
            LineAttribution::new(4, 4, "Dave".to_string(), None),
            LineAttribution::new(5, 5, "Eve".to_string(), None),
        ];

        let char_attrs = line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        assert_eq!(char_attrs.len(), 5);
        assert_eq!(char_attrs[0].author_id, "Alice");
        assert_eq!(char_attrs[1].author_id, "Bob");
        assert_eq!(char_attrs[2].author_id, "Charlie");
        assert_eq!(char_attrs[3].author_id, "Dave");
        assert_eq!(char_attrs[4].author_id, "Eve");
    }

    #[test]
    fn test_line_to_char_round_trip() {
        // Test that converting to line attributions and back preserves information
        let content = "line 1\nline 2\nline 3\n";
        let original_char_attrs = vec![
            Attribution::new(0, 7, "Alice".to_string(), TEST_TS),
            Attribution::new(7, 14, "Bob".to_string(), TEST_TS),
            Attribution::new(14, 21, "Charlie".to_string(), TEST_TS),
        ];

        // Convert to line attributions
        let line_attrs = attributions_to_line_attributions(&original_char_attrs, content);

        // Convert back to character attributions
        let round_trip_char_attrs =
            line_attributions_to_attributions(&line_attrs, content, TEST_TS);

        // Should have same number of attributions
        assert_eq!(round_trip_char_attrs.len(), 3);

        // Should have same authors in same order
        assert_eq!(round_trip_char_attrs[0].author_id, "Alice");
        assert_eq!(round_trip_char_attrs[1].author_id, "Bob");
        assert_eq!(round_trip_char_attrs[2].author_id, "Charlie");

        // Character ranges should match original (line boundaries)
        assert_eq!(round_trip_char_attrs[0].start, 0);
        assert_eq!(round_trip_char_attrs[0].end, 7);
        assert_eq!(round_trip_char_attrs[1].start, 7);
        assert_eq!(round_trip_char_attrs[1].end, 14);
        assert_eq!(round_trip_char_attrs[2].start, 14);
        assert_eq!(round_trip_char_attrs[2].end, 21);
    }

    // ========== LineAttribution Tests ==========

    #[test]
    fn test_line_attribution_simple_single_author() {
        let content = "line 1\nline 2\nline 3\n";
        let attributions = vec![Attribution::new(
            0,
            content.len(),
            "Alice".to_string(),
            TEST_TS,
        )];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].start_line, 1);
        assert_eq!(line_attrs[0].end_line, 3);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_attribution_multiple_authors_distinct_lines() {
        let content = "line 1\nline 2\nline 3\n";
        // Alice: line 1, Bob: line 2, Charlie: line 3
        let attributions = vec![
            Attribution::new(0, 7, "Alice".to_string(), TEST_TS), // "line 1\n"
            Attribution::new(7, 14, "Bob".to_string(), TEST_TS),  // "line 2\n"
            Attribution::new(14, 21, "Charlie".to_string(), TEST_TS), // "line 3\n"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        assert_eq!(line_attrs.len(), 3);
        assert_eq!(line_attrs[0].start_line, 1);
        assert_eq!(line_attrs[0].end_line, 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
        assert_eq!(line_attrs[1].start_line, 2);
        assert_eq!(line_attrs[1].end_line, 2);
        assert_eq!(line_attrs[1].author_id, "Bob");
        assert_eq!(line_attrs[2].start_line, 3);
        assert_eq!(line_attrs[2].end_line, 3);
        assert_eq!(line_attrs[2].author_id, "Charlie");
    }

    #[test]
    fn test_line_attribution_whitespace_doesnt_count() {
        // Test that whitespace is ignored when determining dominant author
        let content = "    code\n";
        let attributions = vec![
            Attribution::new(0, 4, "Alice".to_string(), TEST_TS), // "    " (4 spaces)
            Attribution::new(4, 9, "Bob".to_string(), TEST_TS),   // "code\n"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Alice has 0 non-ws chars, Bob has 4 non-ws chars
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Bob");
    }

    #[test]
    fn test_line_attribution_merging_consecutive_lines() {
        let content = "line 1\nline 2\nline 3\nline 4\n";
        let attributions = vec![
            Attribution::new(0, 7, "Alice".to_string(), TEST_TS), // line 1
            Attribution::new(7, 14, "Alice".to_string(), TEST_TS), // line 2
            Attribution::new(14, 21, "Bob".to_string(), TEST_TS), // line 3
            Attribution::new(21, 28, "Bob".to_string(), TEST_TS), // line 4
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Should merge consecutive lines with same author
        assert_eq!(line_attrs.len(), 2);
        assert_eq!(line_attrs[0].start_line, 1);
        assert_eq!(line_attrs[0].end_line, 2);
        assert_eq!(line_attrs[0].author_id, "Alice");
        assert_eq!(line_attrs[1].start_line, 3);
        assert_eq!(line_attrs[1].end_line, 4);
        assert_eq!(line_attrs[1].author_id, "Bob");
    }

    #[test]
    fn test_line_attribution_overlapping_attributions() {
        let content = "hello world\n";
        let attributions = vec![
            Attribution::new(0, 12, "Alice".to_string(), TEST_TS), // entire line
            Attribution::new(6, 11, "Bob".to_string(), TEST_TS),   // "world"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Alice has "hello world\n" = 10 non-ws chars (hello=5, world=5)
        // Bob has "world" = 5 non-ws chars
        // Alice should win with 10 > 5
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_attribution_empty_content() {
        let content = "";
        let attributions = vec![];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        assert_eq!(line_attrs.len(), 0);
    }

    #[test]
    fn test_line_attribution_no_trailing_newline() {
        let content = "line 1\nline 2";
        let attributions = vec![
            Attribution::new(0, 7, "Alice".to_string(), TEST_TS),
            Attribution::new(7, 13, "Bob".to_string(), TEST_TS),
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        assert_eq!(line_attrs.len(), 2);
        assert_eq!(line_attrs[0].author_id, "Alice");
        assert_eq!(line_attrs[1].author_id, "Bob");
    }

    #[test]
    fn test_line_attribution_realistic_code() {
        let content = r#"fn calculate_sum(a: i32, b: i32) -> i32 {
    let result = a + b;
    println!("Sum: {}", result);
    result
}

fn main() {
    let x = 5;
    let y = 10;
    let sum = calculate_sum(x, y);
    println!("Total: {}", sum);
}
"#;

        // Alice wrote calculate_sum function (lines 1-5)
        // Bob wrote main function (lines 7-12)
        let attributions = vec![
            Attribution::new(0, 89, "Alice".to_string(), TEST_TS),
            Attribution::new(91, content.len(), "Bob".to_string(), TEST_TS),
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Should have Alice's block, then Bob's block
        assert!(line_attrs.len() >= 2);

        // First attribution should be Alice
        assert_eq!(line_attrs[0].author_id, "Alice");
        assert_eq!(line_attrs[0].start_line, 1);

        // Should have a Bob attribution
        let bob_attrs: Vec<_> = line_attrs.iter().filter(|a| a.author_id == "Bob").collect();
        assert!(bob_attrs.len() > 0);
    }

    #[test]
    fn test_line_attribution_mixed_authorship_per_line() {
        let content = "let x = foo() + bar();\n";
        let attributions = vec![
            Attribution::new(0, 8, "Alice".to_string(), TEST_TS), // "let x = "
            Attribution::new(8, 13, "Bob".to_string(), TEST_TS),  // "foo()"
            Attribution::new(13, 16, "Alice".to_string(), TEST_TS), // " + "
            Attribution::new(16, 21, "Charlie".to_string(), TEST_TS), // "bar()"
            Attribution::new(21, 23, "Alice".to_string(), TEST_TS), // ";\n"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Alice: "let x = " (6) + " + " (1) + ";\n" (1) = 8 non-ws chars
        // Bob: "foo()" (5) = 5 non-ws chars
        // Charlie: "bar()" (5) = 5 non-ws chars
        // Alice should win
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_attribution_all_whitespace_line() {
        let content = "code\n    \nmore code\n";
        let attributions = vec![
            Attribution::new(0, 5, "Alice".to_string(), TEST_TS), // "code\n"
            Attribution::new(5, 10, "Bob".to_string(), TEST_TS),  // "    \n" (whitespace line)
            Attribution::new(10, 20, "Charlie".to_string(), TEST_TS), // "more code\n"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Ensure we retain expected attribution for empty lines
        assert_eq!(line_attrs.len(), 3);
        assert_eq!(line_attrs[0].start_line, 1);
        assert_eq!(line_attrs[0].end_line, 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
        assert_eq!(line_attrs[1].start_line, 2);
        assert_eq!(line_attrs[1].end_line, 2);
        assert_eq!(line_attrs[1].author_id, "Bob");
        assert_eq!(line_attrs[2].start_line, 3);
        assert_eq!(line_attrs[2].end_line, 3);
        assert_eq!(line_attrs[2].author_id, "Charlie");
    }

    #[test]
    fn test_line_attribution_helper_methods() {
        let line_attr = LineAttribution::new(5, 10, "Alice".to_string(), None);

        assert_eq!(line_attr.line_count(), 6);
        assert!(!line_attr.is_empty());
        assert!(line_attr.overlaps(8, 12));
        assert!(!line_attr.overlaps(1, 4));
        assert_eq!(line_attr.intersection(8, 12), Some((8, 10)));
        assert_eq!(line_attr.intersection(1, 4), None);
    }

    #[test]
    fn test_move_with_unchanged_content_between() {
        let tracker = AttributionTracker::new();

        // Exact example from the bug report
        let old_content = module_move_old_content();

        let old_attributions = vec![Attribution::new(
            0,
            old_content.len(),
            "A".to_string(),
            TEST_TS,
        )];

        // Move the if block to the end
        let new_content = module_move_new_content();

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "B", TEST_TS)
            .unwrap();

        // TODO Fix bug where the return config\n},\n    })\n  }" is attributed to B (even though it was already there before the move)
        let new_line_attributions =
            attributions_to_line_attributions(&new_attributions, new_content);
        eprintln!("new_line_attributions: {:?}", new_line_attributions);

        // The section "return config\n      },\n    })\n  }" should NOT be attributed to B
        // It should remain attributed to A
        let return_config_pos = new_content.find("return config").unwrap();
        let closing_brace_after_return =
            new_content[return_config_pos..].find("  }").unwrap() + return_config_pos + 3;

        for pos in return_config_pos..closing_brace_after_return {
            let attributed_to_b = new_attributions
                .iter()
                .filter(|a| a.author_id == "B")
                .any(|a| a.start <= pos && a.end > pos);

            assert!(
                !attributed_to_b,
                "Character at position {} should NOT be attributed to B (the section between cut and paste). \
                Character: {:?}",
                pos,
                new_content.chars().nth(pos)
            );
        }
    }

    #[test]
    fn test_move_with_indentation_change_preserves_attribution() {
        let tracker = AttributionTracker::new();

        let old_content = r#"fn main() {
    let value = compute();
    log(value);
}

fn compute() -> i32 {
    42
}
"#;

        let old_attributions = vec![Attribution::new(
            0,
            old_content.len(),
            "A".to_string(),
            TEST_TS,
        )];

        // Move `compute` inside `main` and adjust indentation to match the new scope
        let new_content = r#"fn main() {
    let value = compute();
    fn compute() -> i32 {
        42
    }
    log(value);
}
"#;

        let new_attributions = tracker
            .update_attributions(old_content, new_content, &old_attributions, "B", TEST_TS)
            .unwrap();

        let moved_block = "    fn compute() -> i32 {\n        42\n    }";
        let block_start = new_content
            .find(moved_block)
            .expect("Moved block should exist in new content");
        let block_end = block_start + moved_block.len();

        for idx in block_start..block_end {
            let ch = new_content.as_bytes()[idx] as char;
            if ch.is_whitespace() || ch == '{' || ch == '}' {
                continue;
            }

            let attributed_to_b = new_attributions
                .iter()
                .filter(|a| a.author_id == "B")
                .any(|a| a.start <= idx && a.end > idx);

            assert!(
                !attributed_to_b,
                "Character at position {} ('{}') should remain attributed to A despite indentation change",
                idx, ch
            );
        }
    }

    #[test]
    fn test_move_with_whitespace_gaps_preserves_attribution() {
        let tracker = AttributionTracker::new();

        let old_content = module_move_old_content();

        let old_attributions = vec![Attribution::new(
            0,
            old_content.len(),
            "A".to_string(),
            TEST_TS,
        )];

        // Start with the baseline move scenario and add extra blank lines around the moved block
        let mut new_content = module_move_new_content().to_string();
        new_content = new_content.replace("return config\n      },", "return config\n\n      },");
        new_content = new_content.replace(
            "})\n  }\n  if (typeof nextConfig.webpack === 'function')",
            "})\n\n  }\n  if (typeof nextConfig.webpack === 'function')",
        );

        let new_attributions = tracker
            .update_attributions(old_content, &new_content, &old_attributions, "B", TEST_TS)
            .unwrap();

        let return_config_pos = new_content
            .find("return config")
            .expect("Moved block should exist");
        let closing_brace_after_return =
            new_content[return_config_pos..].find("  }").unwrap() + return_config_pos + 3;

        for idx in return_config_pos..closing_brace_after_return {
            let ch = new_content.as_bytes()[idx] as char;
            if ch.is_whitespace() || ch == '{' || ch == '}' {
                continue;
            }

            let attributed_to_b = new_attributions
                .iter()
                .filter(|a| a.author_id == "B")
                .any(|a| a.start <= idx && a.end > idx);

            assert!(
                !attributed_to_b,
                "Character at position {} ('{}') within moved body block should remain with author A",
                idx, ch
            );
        }
    }

    #[test]
    fn test_line_attribution_strips_leading_trailing_whitespace() {
        // Test that leading and trailing whitespace is stripped from attribution ranges
        let content = "    code    \n";
        let attributions = vec![
            Attribution::new(0, 4, "Alice".to_string(), TEST_TS), // "    " (only whitespace)
            Attribution::new(4, 8, "Bob".to_string(), TEST_TS),   // "code"
            Attribution::new(8, 12, "Charlie".to_string(), TEST_TS), // "    " (only whitespace)
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Only Bob should be attributed (Alice and Charlie have only whitespace)
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Bob");
    }

    #[test]
    fn test_line_attribution_ignores_whitespace_only_ranges_on_non_empty_lines() {
        // Test that ranges containing only whitespace are completely ignored
        let content = "a b c\n";
        let attributions = vec![
            Attribution::new(0, 1, "Alice".to_string(), TEST_TS), // "a"
            Attribution::new(1, 2, "Bob".to_string(), TEST_TS),   // " " (only whitespace)
            Attribution::new(2, 3, "Charlie".to_string(), TEST_TS), // "b"
            Attribution::new(3, 4, "Dave".to_string(), TEST_TS),  // " " (only whitespace)
            Attribution::new(4, 5, "Eve".to_string(), TEST_TS),   // "c"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Bob and Dave should be ignored (only whitespace)
        // Alice: 1 char, Charlie: 1 char, Eve: 1 char (tie, Alice wins alphabetically)
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_attribution_trims_edges_before_counting() {
        // Test that we trim whitespace from edges before counting
        let content = "  code  \n";
        let attributions = vec![
            Attribution::new(0, 8, "Alice".to_string(), TEST_TS), // "  code  " -> trimmed to "code"
            Attribution::new(2, 6, "Bob".to_string(), TEST_TS),   // "code"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Both Alice and Bob have "code" (4 chars each) after trimming
        // Alphabetically, Alice comes first
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_attribution_mixed_with_surrounding_whitespace() {
        // Test with attribution that has whitespace in the middle but not on edges after trim
        let content = "  a b c  \n";
        let attributions = vec![
            Attribution::new(0, 3, "Alice".to_string(), TEST_TS), // "  a" -> trimmed to "a"
            Attribution::new(3, 5, "Bob".to_string(), TEST_TS),   // " b" -> trimmed to "b"
            Attribution::new(5, 9, "Charlie".to_string(), TEST_TS), // " c  " -> trimmed to "c"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // All three have 1 char after trimming, Alice wins alphabetically
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_attribution_whitespace_between_words() {
        // Test that internal whitespace is properly handled but not counted
        let content = "foo   bar\n";
        let attributions = vec![
            Attribution::new(0, 3, "Alice".to_string(), TEST_TS), // "foo"
            Attribution::new(3, 6, "Bob".to_string(), TEST_TS),   // "   " (only whitespace)
            Attribution::new(6, 9, "Charlie".to_string(), TEST_TS), // "bar"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Alice: 3 non-ws chars, Bob: 0 (ignored), Charlie: 3 non-ws chars
        // Tie between Alice and Charlie, Alice wins alphabetically
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_attribution_indentation_only() {
        // Test a line that starts with attribution of only indentation
        let content = "    if (true) {\n";
        let attributions = vec![
            Attribution::new(0, 4, "Alice".to_string(), TEST_TS), // "    " (only whitespace)
            Attribution::new(4, 15, "Bob".to_string(), TEST_TS),  // "if (true) {"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Alice's whitespace-only range should be ignored, Bob should win
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Bob");
    }

    #[test]
    fn test_line_attribution_trim_tabs_and_spaces() {
        // Test that both tabs and spaces are trimmed
        let content = "\t  code  \t\n";
        let attributions = vec![
            Attribution::new(0, 10, "Alice".to_string(), TEST_TS), // "\t  code  \t" -> trimmed to "code"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_line_attribution_empty_after_trim() {
        // Test range that becomes empty after trimming (edge case)
        let content = "a   b\n";
        let attributions = vec![
            Attribution::new(0, 1, "Alice".to_string(), TEST_TS), // "a"
            Attribution::new(1, 4, "Bob".to_string(), TEST_TS),   // "   " (only whitespace)
            Attribution::new(4, 5, "Charlie".to_string(), TEST_TS), // "b"
        ];

        let line_attrs = attributions_to_line_attributions(&attributions, content);

        // Bob's range should be ignored after trimming
        // Alice: 1 char, Charlie: 1 char, Alice wins alphabetically
        assert_eq!(line_attrs.len(), 1);
        assert_eq!(line_attrs[0].author_id, "Alice");
    }

    #[test]
    fn test_ai_human_interleaved_line_attribution_with_discard() {
        let tracker = AttributionTracker::new();

        // Start with base content and no attributions
        let old_content = "Base line";

        // Simulate adding interleaved AI and Human lines

        // Step 1: AI adds a newline and "AI Line 1"
        // "Base line" has no attributions, "\nAI Line 1" is inserted by AI
        let content_v2 = "Base line\nAI Line 1";
        let attributions_v2 = tracker
            .update_attributions(
                old_content,
                content_v2,
                &Vec::new(),
                &CheckpointKind::AiAgent.to_str(),
                TEST_TS,
            )
            .unwrap();

        // Step 2: Human adds "Human Line 1"
        let content_v3 = "Base line\nAI Line 1\nHuman Line 1";
        let attributions_v3 = tracker
            .update_attributions(
                content_v2,
                content_v3,
                &attributions_v2,
                &CheckpointKind::Human.to_str(),
                TEST_TS,
            )
            .unwrap();

        // Step 3: AI adds "AI Line 2"
        let content_v4 = "Base line\nAI Line 1\nHuman Line 1\nAI Line 2";
        let attributions_v4 = tracker
            .update_attributions(
                content_v3,
                content_v4,
                &attributions_v3,
                &CheckpointKind::AiAgent.to_str(),
                TEST_TS,
            )
            .unwrap();

        // Convert to line attributions
        let line_attrs = attributions_to_line_attributions(&attributions_v4, content_v4);

        // Expected result after discarding Human:
        // Line 1 ("Base line") - was Human, now discarded, should have no attribution
        // Line 2 ("AI Line 1") - AI, should be attributed to AI
        // Line 3 ("Human Line 1") - was Human, now discarded, should have no attribution
        // Line 4 ("AI Line 2") - AI, should be attributed to AI
        for line_attr in line_attrs {
            match line_attr.start_line {
                1 | 3 => {
                    assert_eq!(line_attr.author_id, CheckpointKind::Human.to_str());
                }
                2 | 4 => {
                    assert_eq!(line_attr.author_id, CheckpointKind::AiAgent.to_str());
                }
                _ => {
                    panic!(
                        "Unexpected line number: {:?}. Expected 1, 2, 3, or 4. Got: {:?}",
                        line_attr.start_line, line_attr
                    );
                }
            }
        }
    }

    #[test]
    fn test_human_replaces_ai_line() {
        // 1. Initial commit has "Line 1\nLine 2" (Human)
        // 2. AI replaces line 2 with "AI modification of line 2"
        // 3. Human replaces line 2 with "Human modification of line 2"
        // After step 3, line 2 should be attributed to Human (no AI attribution)

        let tracker = AttributionTracker::new();

        // Step 1: Initial state "Line 1\nLine 2" with no attributions (Human)
        let v1_content = "Line 1\nLine 2\n";
        let v1_attributions = Vec::new();

        // Step 2: AI modifies line 2
        let v2_content = "Line 1\nAI modification of line 2\n";
        let v2_attributions = tracker
            .update_attributions(
                v1_content,
                v2_content,
                &v1_attributions,
                &CheckpointKind::AiAgent.to_str(),
                TEST_TS,
            )
            .unwrap();

        let v2_line_attrs = attributions_to_line_attributions(&v2_attributions, v2_content);

        // After discarding Human attributions, only line 2 should be attributed to AI
        for line_attr in v2_line_attrs {
            match line_attr.start_line {
                2 => {
                    assert_eq!(line_attr.author_id, CheckpointKind::AiAgent.to_str());
                }
                _ => {
                    panic!(
                        "Unexpected line number: {:?}. Expected 2. Got: {:?}",
                        line_attr.start_line, line_attr
                    );
                }
            }
        }

        // Step 3: Human replaces line 2 with different content
        let v3_content = "Line 1\nHuman modification of line 2\n";
        let v3_attributions = tracker
            .update_attributions(
                v2_content,
                v3_content,
                &v2_attributions,
                &CheckpointKind::Human.to_str(),
                TEST_TS,
            )
            .unwrap();

        let v3_line_attrs = attributions_to_line_attributions(&v3_attributions, v3_content);

        // Assert that line 2 is attributed to Human
        for line_attr in v3_line_attrs {
            match line_attr.start_line {
                2 => {
                    assert_eq!(line_attr.author_id, CheckpointKind::Human.to_str());
                }
                _ => {
                    panic!(
                        "Unexpected line number: {:?}. Expected 2. Got: {:?}",
                        line_attr.start_line, line_attr
                    );
                }
            }
        }
    }

    #[test]
    fn test_add_multiple_lines() {
        // Simulates: Human writes 3 lines, then AI adds 2 lines
        let tracker = AttributionTracker::new();

        // Step 1: Human creates file
        let v1_content = "Line 1 from human\nLine 2 from human\nLine 3 from human\n||__AI LINE__ PENDING__||\n||__AI LINE__ PENDING__||";
        let v1_attributions = tracker
            .update_attributions(
                "",
                v1_content,
                &Vec::new(),
                &CheckpointKind::Human.to_str(),
                TEST_TS,
            )
            .unwrap();

        // Step 2: Replaces the two lines at the end
        let v2_content = "Line 1 from human\nLine 2 from human\nLine 3 from human\nLine 4 from AI\nLine 5 from AI";
        let v2_attributions = tracker
            .update_attributions(
                v1_content,
                v2_content,
                &v1_attributions,
                &CheckpointKind::AiAgent.to_str(),
                TEST_TS + 1,
            )
            .unwrap();

        let v2_line_attrs = attributions_to_line_attributions(&v2_attributions, v2_content);

        // Lines 4-5 should be attributed to AI
        assert!(
            v2_line_attrs.len() >= 1,
            "Should have at least 1 line attribution. Got: {:?}",
            v2_line_attrs
        );

        // Find the AI attribution
        let ai_attr = v2_line_attrs
            .iter()
            .find(|attr| attr.author_id == CheckpointKind::AiAgent.to_str());
        assert!(
            ai_attr.is_some(),
            "Should have AI attribution. Got: {:?}",
            v2_line_attrs
        );

        let ai_attr = ai_attr.unwrap();
        assert_eq!(
            ai_attr.start_line, 4,
            "AI attribution should start at line 4"
        );
        assert_eq!(ai_attr.end_line, 5, "AI attribution should end at line 5");
    }

    #[test]
    fn test_replace_one_human_line_with_ai_line() {
        // Simulates: Human writes 4 lines, then AI replaces one of them with an AI line
        let tracker = AttributionTracker::new();

        // Step 1: Human creates file
        let v1_content = "Line 1\nLine 2\n||__AI LINE__ PENDING__||\nLine 4";
        let v1_attributions = tracker
            .update_attributions(
                "",
                v1_content,
                &Vec::new(),
                &CheckpointKind::Human.to_str(),
                TEST_TS,
            )
            .unwrap();

        // Step 2: Replaces the two lines at the end
        let v2_content = "Line 1\nLine 2\nLine 3\nLine 4";
        let v2_attributions = tracker
            .update_attributions(
                v1_content,
                v2_content,
                &v1_attributions,
                &CheckpointKind::AiAgent.to_str(),
                TEST_TS + 1,
            )
            .unwrap();

        let v2_line_attrs = attributions_to_line_attributions(&v2_attributions, v2_content);

        // Lines 4-5 should be attributed to AI
        assert!(
            v2_line_attrs.len() >= 1,
            "Should have at least 1 line attribution. Got: {:?}",
            v2_line_attrs
        );

        // Check that line 3 is attributed to AI
        for attr in v2_line_attrs {
            match attr.start_line {
                3 => {
                    assert_eq!(attr.author_id, CheckpointKind::AiAgent.to_str());
                    assert_eq!(attr.end_line, 3);
                    break;
                }
                _ => {
                    assert_eq!(attr.author_id, CheckpointKind::Human.to_str());
                }
            }
        }
    }

    // ========== Unattributed Attribution Tests ==========

    #[test]
    fn test_attribute_unattributed_lines_empty_content() {
        let tracker = AttributionTracker::new();
        let content = "";
        let prev_attributions = vec![];

        let result =
            tracker.attribute_unattributed_ranges(content, &prev_attributions, "Alice", TEST_TS);

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_attribute_unattributed_lines_no_previous_attributions() {
        let tracker = AttributionTracker::new();
        let content = "Hello world";
        let prev_attributions = vec![];

        let result =
            tracker.attribute_unattributed_ranges(content, &prev_attributions, "Alice", TEST_TS);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start, 0);
        assert_eq!(result[0].end, 11);
        assert_eq!(result[0].author_id, "Alice");
        assert_eq!(result[0].ts, TEST_TS);
    }

    #[test]
    fn test_attribute_unattributed_lines_partially_attributed() {
        let tracker = AttributionTracker::new();
        let content = "Hello beautiful world";
        let prev_attributions = vec![
            Attribution::new(0, 6, "Bob".to_string(), TEST_TS), // "Hello "
            Attribution::new(15, 21, "Charlie".to_string(), TEST_TS), // " world"
        ];

        let result = tracker.attribute_unattributed_ranges(
            content,
            &prev_attributions,
            "Alice",
            TEST_TS + 1,
        );

        // Should have original attributions plus one new attribution for "beautiful"
        assert_eq!(result.len(), 3);

        // Check that "beautiful" (chars 6-15) is attributed to Alice
        let alice_attrs: Vec<_> = result.iter().filter(|a| a.author_id == "Alice").collect();
        assert_eq!(alice_attrs.len(), 1);
        assert_eq!(alice_attrs[0].start, 6);
        assert_eq!(alice_attrs[0].end, 15);
        assert_eq!(alice_attrs[0].ts, TEST_TS + 1);

        // Check that original attributions are preserved
        let bob_attrs: Vec<_> = result.iter().filter(|a| a.author_id == "Bob").collect();
        let charlie_attrs: Vec<_> = result.iter().filter(|a| a.author_id == "Charlie").collect();
        assert_eq!(bob_attrs.len(), 1);
        assert_eq!(charlie_attrs.len(), 1);
    }

    #[test]
    fn test_attribute_unattributed_lines_multiple_gaps() {
        let tracker = AttributionTracker::new();
        let content = "A B C D E F";
        let prev_attributions = vec![
            Attribution::new(0, 1, "Bob".to_string(), TEST_TS), // "A"
            Attribution::new(4, 5, "Charlie".to_string(), TEST_TS), // "C"
            Attribution::new(8, 9, "Dave".to_string(), TEST_TS), // "E"
        ];

        let result = tracker.attribute_unattributed_ranges(
            content,
            &prev_attributions,
            "Alice",
            TEST_TS + 1,
        );

        // Should have 3 original attributions + 3 new Alice attributions for gaps
        assert_eq!(result.len(), 6);

        // Check Alice attributions for the gaps
        let alice_attrs: Vec<_> = result.iter().filter(|a| a.author_id == "Alice").collect();
        assert_eq!(alice_attrs.len(), 3);

        // Gap 1: " B " (chars 1-4)
        // Gap 2: " D " (chars 5-8)
        // Gap 3: " F" (chars 9-11)
        let gap_ranges: Vec<_> = alice_attrs.iter().map(|a| (a.start, a.end)).collect();
        assert!(gap_ranges.contains(&(1, 4)), "Should have gap 1-4");
        assert!(gap_ranges.contains(&(5, 8)), "Should have gap 5-8");
        assert!(gap_ranges.contains(&(9, 11)), "Should have gap 9-11");
    }

    #[test]
    fn test_attribute_unattributed_lines_contiguous_gaps() {
        let tracker = AttributionTracker::new();
        let content = "ABC";
        let prev_attributions = vec![
            Attribution::new(0, 1, "Bob".to_string(), TEST_TS), // "A"
        ];

        let result = tracker.attribute_unattributed_ranges(
            content,
            &prev_attributions,
            "Alice",
            TEST_TS + 1,
        );

        // Should have 1 original attribution + 1 new attribution for "BC"
        assert_eq!(result.len(), 2);

        let alice_attrs: Vec<_> = result.iter().filter(|a| a.author_id == "Alice").collect();
        assert_eq!(alice_attrs.len(), 1);
        assert_eq!(alice_attrs[0].start, 1);
        assert_eq!(alice_attrs[0].end, 3);
    }

    #[test]
    fn test_attribute_unattributed_lines_fully_attributed() {
        let tracker = AttributionTracker::new();
        let content = "Hello";
        let prev_attributions = vec![Attribution::new(0, 5, "Bob".to_string(), TEST_TS)];

        let result = tracker.attribute_unattributed_ranges(
            content,
            &prev_attributions,
            "Alice",
            TEST_TS + 1,
        );

        // Should have only the original attribution, no new ones
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].author_id, "Bob");
    }

    #[test]
    fn test_attribute_unattributed_lines_multiline_content() {
        let tracker = AttributionTracker::new();
        let content = "Line 1\nLine 2\nLine 3\n";
        let prev_attributions = vec![
            Attribution::new(0, 7, "Bob".to_string(), TEST_TS), // "Line 1\n"
            // Line 2 is unattributed
            Attribution::new(14, 21, "Charlie".to_string(), TEST_TS), // "Line 3\n"
        ];

        let result = tracker.attribute_unattributed_ranges(
            content,
            &prev_attributions,
            "Alice",
            TEST_TS + 1,
        );

        // Should have 2 original attributions + 1 new attribution for "Line 2\n"
        assert_eq!(result.len(), 3);

        let alice_attrs: Vec<_> = result.iter().filter(|a| a.author_id == "Alice").collect();
        assert_eq!(alice_attrs.len(), 1);
        assert_eq!(alice_attrs[0].start, 7);
        assert_eq!(alice_attrs[0].end, 14);
    }

    #[test]
    fn test_attribute_unattributed_lines_preserves_timestamps() {
        let tracker = AttributionTracker::new();
        let content = "Hello world";
        let prev_attributions = vec![Attribution::new(0, 6, "Bob".to_string(), 1000)];

        let result =
            tracker.attribute_unattributed_ranges(content, &prev_attributions, "Alice", 2000);

        assert_eq!(result.len(), 2);

        // Check that original timestamp is preserved
        let bob_attr = result.iter().find(|a| a.author_id == "Bob").unwrap();
        assert_eq!(bob_attr.ts, 1000);

        // Check that new attribution has correct timestamp
        let alice_attr = result.iter().find(|a| a.author_id == "Alice").unwrap();
        assert_eq!(alice_attr.ts, 2000);
    }

    #[test]
    fn test_attribute_unattributed_lines_complex_overlapping() {
        let tracker = AttributionTracker::new();
        let content = "ABCDEFGHIJ";
        let prev_attributions = vec![
            Attribution::new(1, 3, "Bob".to_string(), TEST_TS), // "BC"
            Attribution::new(4, 6, "Charlie".to_string(), TEST_TS), // "EF"
            Attribution::new(7, 9, "Dave".to_string(), TEST_TS), // "HI"
        ];

        let result = tracker.attribute_unattributed_ranges(
            content,
            &prev_attributions,
            "Alice",
            TEST_TS + 1,
        );

        // Should have 3 original attributions + 4 new Alice attributions for gaps
        assert_eq!(result.len(), 7);

        let alice_attrs: Vec<_> = result.iter().filter(|a| a.author_id == "Alice").collect();
        assert_eq!(alice_attrs.len(), 4);

        // Gap 1: "A" (char 0)
        // Gap 2: "D" (char 3)
        // Gap 3: "G" (char 6)
        // Gap 4: "J" (char 9)
        let gap_ranges: Vec<_> = alice_attrs.iter().map(|a| (a.start, a.end)).collect();
        assert!(gap_ranges.contains(&(0, 1)), "Should have gap 0-1");
        assert!(gap_ranges.contains(&(3, 4)), "Should have gap 3-4");
        assert!(gap_ranges.contains(&(6, 7)), "Should have gap 6-7");
        assert!(gap_ranges.contains(&(9, 10)), "Should have gap 9-10");
    }

    // Test data for mobile nav scenarios
    fn mobile_nav_test_blocks() -> (&'static str, &'static str, &'static str) {
        let human_block_1 = r#""use client"

import * as React from "react"
import Link, { LinkProps } from "next/link"
import { useRouter } from "next/navigation"

import { PAGES_NEW } from "@/lib/docs"
import { showMcpDocs } from "@/lib/flags"
import { source } from "@/lib/source"
import { cn } from "@/lib/utils"
import { Button } from "@/registry/new-york-v4/ui/button"
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/registry/new-york-v4/ui/popover""#;

        let human_block_2 = r#"const TOP_LEVEL_SECTIONS = [
  { name: "Get Started", href: "/docs" },
  {
    name: "Components",
    href: "/docs/components",
  },
  {
    name: "Registry",
    href: "/docs/registry",
  },
  {
    name: "MCP Server",
    href: "/docs/mcp",
  },
  {
    name: "Forms",
    href: "/docs/forms",
  },
  {
    name: "Changelog",
    href: "/docs/changelog",
  },
]

export function MobileNav({
  tree,
  items,
  className,
}: {
  tree: typeof source.pageTree
  items: { href: string; label: string }[]
  className?: string
}) {
  const [open, setOpen] = React.useState(false)

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          className={cn(
            "extend-touch-target h-8 touch-manipulation items-center justify-start gap-2.5 !p-0 hover:bg-transparent focus-visible:bg-transparent focus-visible:ring-0 active:bg-transparent dark:hover:bg-transparent",
            className
          )}
        >
          <div className="relative flex h-8 w-4 items-center justify-center">
            <div className="relative size-4">
              <span
                className={cn(
                  "bg-foreground absolute left-0 block h-0.5 w-4 transition-all duration-100",
                  open ? "top-[0.4rem] -rotate-45" : "top-1"
                )}
              />
              <span
                className={cn(
                  "bg-foreground absolute left-0 block h-0.5 w-4 transition-all duration-100",
                  open ? "top-[0.4rem] rotate-45" : "top-2.5"
                )}
              />
            </div>
            <span className="sr-only">Toggle Menu</span>
          </div>
          <span className="flex h-8 items-center text-lg leading-none font-medium">
            Menu
          </span>
        </Button>
      </PopoverTrigger>
      <PopoverContent
        className="bg-background/90 no-scrollbar h-(--radix-popper-available-height) w-(--radix-popper-available-width) overflow-y-auto rounded-none border-none p-0 shadow-none backdrop-blur duration-100"
        align="start"
        side="bottom"
        alignOffset={-16}
        sideOffset={14}
      >
        <div className="flex flex-col gap-12 overflow-auto px-6 py-6">
          <div className="flex flex-col gap-4">
            <div className="text-muted-foreground text-sm font-medium">
              Menu
            </div>
            <div className="flex flex-col gap-3">
              <MobileLink href="/" onOpenChange={setOpen}>
                Home
              </MobileLink>
              {items.map((item, index) => (
                <MobileLink key={index} href={item.href} onOpenChange={setOpen}>
                  {item.label}
                </MobileLink>
              ))}
            </div>
          </div>
          <div className="flex flex-col gap-4">
            <div className="text-muted-foreground text-sm font-medium">
              Sections
            </div>
            <div className="flex flex-col gap-3">
              {TOP_LEVEL_SECTIONS.map(({ name, href }) => {
                if (!showMcpDocs && href.includes("/mcp")) {
                  return null
                }
                return (
                  <MobileLink key={name} href={href} onOpenChange={setOpen}>
                    {name}
                  </MobileLink>
                )
              })}
            </div>
          </div>
          <div className="flex flex-col gap-8">
            {tree?.children?.map((group, index) => {
              if (group.type === "folder") {
                return (
                  <div key={index} className="flex flex-col gap-4">
                    <div className="text-muted-foreground text-sm font-medium">
                      {group.name}
                    </div>
                    <div className="flex flex-col gap-3">
                      {group.children.map((item) => {
                        if (item.type === "page") {
                          if (!showMcpDocs && item.url.includes("/mcp")) {
                            return null
                          }
                          return (
                            <MobileLink
                              href={item.url}
                              onOpenChange={setOpen}
                              className="flex items-center gap-2"
                            >
                              {item.name}{" "}
                              {PAGES_NEW.includes(item.url) && (
                                <span className="flex size-2 rounded-full bg-blue-500" />
                              )}
                            </MobileLink>
                          )
                        }
                      })}
                    </div>
                  </div>
                )
              }
            })}
          </div>
        </div>
      </PopoverContent>
    </Popover>
  )
}"#;

        let ai_block = r#"function MobileLink({
    href,
    onOpenChange,
    className,
    children,
    ...props
  }: LinkProps & {
    onOpenChange?: (open: boolean) => void
    children: React.ReactNode
    className?: string
  }) {

    const router = useRouter()
    return (
      <Link
        href={href}
        onClick={() => {
          router.push(href.toString())
          onOpenChange?.(false)
        }}
        className={cn("text-2xl font-medium", className)}
        {...props}
      >

        {children}

      </Link>
    )
  }"#;

        (human_block_1, human_block_2, ai_block)
    }

    #[test]
    fn test_mobile_nav_move_scenario() {
        // This test mimics the behavior from scripts/mobile-nav-test.ts
        // where an AI block is added at the bottom, then moved to the middle
        let tracker = AttributionTracker::new();
        let (human_block_1, human_block_2, ai_block) = mobile_nav_test_blocks();

        // Step 1: Two human blocks only
        let step1_content = format!("{}\n\n{}", human_block_1, human_block_2);
        let step1_attributions = vec![Attribution::new(
            0,
            step1_content.len(),
            "human".to_string(),
            TEST_TS,
        )];

        // Step 2: AI block added at the bottom
        let step2_content = format!("{}\n\n{}\n\n{}", human_block_1, human_block_2, ai_block);
        let step2_attributions = tracker
            .update_attributions(
                &step1_content,
                &step2_content,
                &step1_attributions,
                "ai",
                TEST_TS + 1,
            )
            .unwrap();

        // Verify AI block is attributed to "ai"
        let ai_block_start = step2_content.find("function MobileLink").unwrap();
        let ai_block_end = ai_block_start + ai_block.len();

        let ai_chars_in_step2: usize = step2_attributions
            .iter()
            .filter(|a| a.author_id == "ai")
            .map(|a| {
                let overlap_start = a.start.max(ai_block_start);
                let overlap_end = a.end.min(ai_block_end);
                if overlap_start < overlap_end {
                    overlap_end - overlap_start
                } else {
                    0
                }
            })
            .sum();

        assert!(
            ai_chars_in_step2 > ai_block.len() / 2,
            "AI block should be mostly attributed to 'ai' in step 2, got {} chars out of {}",
            ai_chars_in_step2,
            ai_block.len()
        );

        // Step 3: Move AI block to the middle (Human 1 + AI + Human 2)
        let step3_content = format!("{}\n\n{}\n\n{}", human_block_1, ai_block, human_block_2);
        let step3_attributions = tracker
            .update_attributions(
                &step2_content,
                &step3_content,
                &step2_attributions,
                "human",
                TEST_TS + 2,
            )
            .unwrap();

        // Verify the AI block MOVED and retained attribution
        let ai_block_new_start = step3_content.find("function MobileLink").unwrap();
        let ai_block_new_end = ai_block_new_start + ai_block.len();

        let ai_chars_in_step3: usize = step3_attributions
            .iter()
            .filter(|a| a.author_id == "ai")
            .map(|a| {
                let overlap_start = a.start.max(ai_block_new_start);
                let overlap_end = a.end.min(ai_block_new_end);
                if overlap_start < overlap_end {
                    overlap_end - overlap_start
                } else {
                    0
                }
            })
            .sum();

        // The critical assertion: AI block should retain AI attribution after the move
        assert!(
            ai_chars_in_step3 > ai_block.len() / 2,
            "AI block should retain 'ai' attribution after move, got {} chars out of {} ({}%)",
            ai_chars_in_step3,
            ai_block.len(),
            (ai_chars_in_step3 * 100) / ai_block.len()
        );

        // Additional verification: human blocks should remain attributed to human
        let human_block_2_start = step3_content.rfind("const TOP_LEVEL_SECTIONS").unwrap();
        let human_chars_in_block2: usize = step3_attributions
            .iter()
            .filter(|a| a.author_id == "human")
            .filter(|a| a.start >= human_block_2_start)
            .map(|a| a.len())
            .sum();

        assert!(
            human_chars_in_block2 > human_block_2.len() / 2,
            "Human block 2 should retain 'human' attribution, got {} chars out of {}",
            human_chars_in_block2,
            human_block_2.len()
        );
    }

    #[test]
    fn test_mobile_nav_move_scenario_with_indentation() {
        // This test mimics the behavior from scripts/mobile-nav-test.ts
        // where an AI block is added at the bottom, then moved to the middle
        let tracker = AttributionTracker::new();
        let (human_block_1, human_block_2, ai_block) = mobile_nav_test_blocks();

        // Step 1: Two human blocks only
        let step1_content = format!("{}\n\n{}", human_block_1, human_block_2);
        let step1_attributions = vec![Attribution::new(
            0,
            step1_content.len(),
            "human".to_string(),
            TEST_TS,
        )];

        // Step 2: AI block added at the bottom
        let step2_content = format!("{}\n\n{}\n\n{}", human_block_1, human_block_2, ai_block);
        let step2_attributions = tracker
            .update_attributions(
                &step1_content,
                &step2_content,
                &step1_attributions,
                "ai",
                TEST_TS + 1,
            )
            .unwrap();

        // Verify AI block is attributed to "ai"
        let ai_block_start = step2_content.find("function MobileLink").unwrap();
        let ai_block_end = ai_block_start + ai_block.len();

        let ai_chars_in_step2: usize = step2_attributions
            .iter()
            .filter(|a| a.author_id == "ai")
            .map(|a| {
                let overlap_start = a.start.max(ai_block_start);
                let overlap_end = a.end.min(ai_block_end);
                if overlap_start < overlap_end {
                    overlap_end - overlap_start
                } else {
                    0
                }
            })
            .sum();

        assert!(
            ai_chars_in_step2 > ai_block.len() / 2,
            "AI block should be mostly attributed to 'ai' in step 2, got {} chars out of {}",
            ai_chars_in_step2,
            ai_block.len()
        );

        // Step 3: Move AI block to the middle (Human 1 + AI + Human 2)
        let step3_content = format!(
            "{}\n\n{}\n\n{}",
            human_block_1,
            ai_block.replace("\n", "\n    "),
            human_block_2
        );
        let step3_attributions = tracker
            .update_attributions(
                &step2_content,
                &step3_content,
                &step2_attributions,
                "human",
                TEST_TS + 2,
            )
            .unwrap();

        // Verify the AI block MOVED and retained attribution
        let ai_block_new_start = step3_content.find("function MobileLink").unwrap();
        let ai_block_new_end = ai_block_new_start + ai_block.len();

        let ai_chars_in_step3: usize = step3_attributions
            .iter()
            .filter(|a| a.author_id == "ai")
            .map(|a| {
                let overlap_start = a.start.max(ai_block_new_start);
                let overlap_end = a.end.min(ai_block_new_end);
                if overlap_start < overlap_end {
                    overlap_end - overlap_start
                } else {
                    0
                }
            })
            .sum();

        // The critical assertion: AI block should retain AI attribution after the move
        assert!(
            ai_chars_in_step3 > ai_block.len() / 2,
            "AI block should retain 'ai' attribution after move, got {} chars out of {} ({}%)",
            ai_chars_in_step3,
            ai_block.len(),
            (ai_chars_in_step3 * 100) / ai_block.len()
        );

        // Additional verification: human blocks should remain attributed to human
        let human_block_2_start = step3_content.rfind("const TOP_LEVEL_SECTIONS").unwrap();
        let human_chars_in_block2: usize = step3_attributions
            .iter()
            .filter(|a| a.author_id == "human")
            .filter(|a| a.start >= human_block_2_start)
            .map(|a| a.len())
            .sum();

        assert!(
            human_chars_in_block2 > human_block_2.len() / 2,
            "Human block 2 should retain 'human' attribution, got {} chars out of {}",
            human_chars_in_block2,
            human_block_2.len()
        );
    }
}
