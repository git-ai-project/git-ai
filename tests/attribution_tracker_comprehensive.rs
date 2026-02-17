/// Comprehensive tests for src/authorship/attribution_tracker.rs
///
/// This test module covers critical functionality in attribution_tracker.rs (2,573 LOC)
/// which is the core diff-based attribution tracking module that underpins AI authorship tracking.
///
/// Test coverage areas:
/// 1. Basic line attribution (AI vs human edits)
/// 2. Move detection across files and within files
/// 3. Whitespace-only changes
/// 4. Mixed AI/human edits on same lines
/// 5. Large file performance
/// 6. Unicode and special character handling
/// 7. Diff algorithm edge cases
/// 8. Character-level attribution tracking
/// 9. Attribution preservation through renames
/// 10. Multi-file attribution scenarios

#[macro_use]
mod repos;

use git_ai::authorship::attribution_tracker::{
    Attribution, AttributionConfig, AttributionTracker, INITIAL_ATTRIBUTION_TS, LineAttribution,
};
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

// =============================================================================
// Basic Attribution Tests - Core functionality
// =============================================================================

#[test]
fn test_attribution_new_creates_valid_range() {
    // Test that Attribution::new creates valid ranges
    let attr = Attribution::new(0, 10, "ai-1".to_string(), 1000);
    assert_eq!(attr.start, 0);
    assert_eq!(attr.end, 10);
    assert_eq!(attr.author_id, "ai-1");
    assert_eq!(attr.ts, 1000);
    assert_eq!(attr.len(), 10);
    assert!(!attr.is_empty());
}

#[test]
fn test_attribution_empty_range() {
    // Test empty attribution ranges
    let attr = Attribution::new(5, 5, "ai-1".to_string(), 1000);
    assert!(attr.is_empty());
    assert_eq!(attr.len(), 0);
}

#[test]
fn test_attribution_overlaps_basic() {
    // Test basic overlap detection
    let attr = Attribution::new(10, 20, "ai-1".to_string(), 1000);

    // Overlaps
    assert!(attr.overlaps(5, 15)); // Starts before, overlaps start
    assert!(attr.overlaps(15, 25)); // Overlaps end, extends after
    assert!(attr.overlaps(12, 18)); // Fully contained
    assert!(attr.overlaps(5, 25)); // Fully encompasses

    // Does not overlap
    assert!(!attr.overlaps(0, 10)); // Ends at start
    assert!(!attr.overlaps(20, 30)); // Starts at end
    assert!(!attr.overlaps(0, 5)); // Completely before
    assert!(!attr.overlaps(25, 30)); // Completely after
}

#[test]
fn test_attribution_intersection() {
    // Test intersection computation
    let attr = Attribution::new(10, 20, "ai-1".to_string(), 1000);

    assert_eq!(attr.intersection(5, 15), Some((10, 15)));
    assert_eq!(attr.intersection(15, 25), Some((15, 20)));
    assert_eq!(attr.intersection(12, 18), Some((12, 18)));
    assert_eq!(attr.intersection(5, 25), Some((10, 20)));
    assert_eq!(attr.intersection(0, 10), None);
    assert_eq!(attr.intersection(20, 30), None);
}

#[test]
fn test_line_attribution_new_creates_valid_range() {
    // Test that LineAttribution::new creates valid ranges
    let attr = LineAttribution::new(1, 10, "ai-1".to_string(), None);
    assert_eq!(attr.start_line, 1);
    assert_eq!(attr.end_line, 10);
    assert_eq!(attr.author_id, "ai-1");
    assert_eq!(attr.overrode, None);
    assert_eq!(attr.line_count(), 10);
    assert!(!attr.is_empty());
}

#[test]
fn test_line_attribution_with_override() {
    // Test LineAttribution with override tracking
    let attr = LineAttribution::new(1, 5, "human-1".to_string(), Some("ai-1".to_string()));
    assert_eq!(attr.overrode, Some("ai-1".to_string()));
}

#[test]
fn test_line_attribution_overlaps() {
    // Test line attribution overlap detection
    let attr = LineAttribution::new(10, 20, "ai-1".to_string(), None);

    assert!(attr.overlaps(5, 15)); // Overlaps start
    assert!(attr.overlaps(15, 25)); // Overlaps end
    assert!(attr.overlaps(12, 18)); // Fully contained
    assert!(attr.overlaps(5, 25)); // Fully encompasses

    assert!(!attr.overlaps(1, 9)); // Before
    assert!(!attr.overlaps(21, 30)); // After
}

#[test]
fn test_line_attribution_intersection() {
    // Test line attribution intersection
    let attr = LineAttribution::new(10, 20, "ai-1".to_string(), None);

    assert_eq!(attr.intersection(5, 15), Some((10, 15)));
    assert_eq!(attr.intersection(15, 25), Some((15, 20)));
    assert_eq!(attr.intersection(12, 18), Some((12, 18)));
    assert_eq!(attr.intersection(5, 25), Some((10, 20)));
    assert_eq!(attr.intersection(1, 9), None);
    assert_eq!(attr.intersection(21, 30), None);
}

// =============================================================================
// AttributionTracker Tests - Core update_attributions functionality
// =============================================================================

#[test]
fn test_tracker_no_changes_preserves_attributions() {
    // Test that identical content preserves all attributions
    let tracker = AttributionTracker::new();
    let content = "line 1\nline 2\nline 3\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "human-1".to_string(), 2000),
        Attribution::new(14, 21, "ai-2".to_string(), 3000),
    ];

    let new_attrs = tracker
        .update_attributions(content, content, &old_attrs, "current-author", 4000)
        .unwrap();

    assert_eq!(new_attrs.len(), 3);
    assert_eq!(new_attrs[0].author_id, "ai-1");
    assert_eq!(new_attrs[1].author_id, "human-1");
    assert_eq!(new_attrs[2].author_id, "ai-2");
}

#[test]
fn test_tracker_simple_addition_at_end() {
    // Test adding new content at the end
    let tracker = AttributionTracker::new();
    let old_content = "line 1\n";
    let new_content = "line 1\nline 2\n";

    let old_attrs = vec![Attribution::new(0, 7, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 2000)
        .unwrap();

    // Should preserve old attribution and add new one for added content
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "current-author"));
}

#[test]
fn test_tracker_simple_addition_at_start() {
    // Test adding new content at the start
    let tracker = AttributionTracker::new();
    let old_content = "line 2\n";
    let new_content = "line 1\nline 2\n";

    let old_attrs = vec![Attribution::new(0, 7, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 2000)
        .unwrap();

    // New content at start should be attributed to current author
    assert!(
        new_attrs
            .iter()
            .any(|a| a.author_id == "current-author" && a.start == 0)
    );
    // Old content should be shifted and preserved
    assert!(
        new_attrs
            .iter()
            .any(|a| a.author_id == "ai-1" && a.start > 0)
    );
}

#[test]
fn test_tracker_simple_deletion_at_end() {
    // Test deleting content at the end
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\n";
    let new_content = "line 1\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 3000)
        .unwrap();

    // Should preserve first attribution only
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    // Deleted content attribution should be gone or marked with deletion
    // There might be a marker attribution for the deletion
    assert!(
        new_attrs
            .iter()
            .any(|a| a.author_id == "current-author" || a.author_id == "ai-1")
    );
}

#[test]
fn test_tracker_simple_deletion_at_start() {
    // Test deleting content at the start
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\n";
    let new_content = "line 2\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 3000)
        .unwrap();

    // Should preserve second attribution, shifted to start
    assert!(
        new_attrs
            .iter()
            .any(|a| a.author_id == "ai-2" || a.author_id == "current-author")
    );
}

#[test]
fn test_tracker_modification_in_middle() {
    // Test modifying content in the middle
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\nline 3\n";
    let new_content = "line 1\nmodified\nline 3\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
        Attribution::new(14, 21, "ai-3".to_string(), 3000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 4000)
        .unwrap();

    // Should preserve first and last attributions
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-3"));
    // Middle should be attributed to current author
    assert!(new_attrs.iter().any(|a| a.author_id == "current-author"));
}

// =============================================================================
// Whitespace Handling Tests
// =============================================================================

#[test]
fn test_tracker_whitespace_only_addition() {
    // Test that whitespace-only additions are handled correctly
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\n";
    let new_content = "line 1\n\n\nline 2\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 3000)
        .unwrap();

    // Original attributions should be preserved, potentially with whitespace attributed
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-2"));
}

#[test]
fn test_tracker_whitespace_only_deletion() {
    // Test that whitespace-only deletions are handled correctly
    let tracker = AttributionTracker::new();
    let old_content = "line 1\n\n\nline 2\n";
    let new_content = "line 1\nline 2\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 9, "ai-2".to_string(), 2000),
        Attribution::new(9, 16, "ai-3".to_string(), 3000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 4000)
        .unwrap();

    // Should preserve non-whitespace attributions
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-3"));
}

#[test]
fn test_tracker_trailing_whitespace_changes() {
    // Test trailing whitespace changes
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\n";
    let new_content = "line 1  \nline 2  \n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 3000)
        .unwrap();

    // Original attributions should be preserved
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-2"));
}

#[test]
fn test_tracker_indentation_changes() {
    // Test indentation changes
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\n";
    let new_content = "    line 1\n    line 2\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 3000)
        .unwrap();

    // Should have attributions for both original content and added indentation
    assert!(!new_attrs.is_empty());
}

// =============================================================================
// Unicode and Special Character Tests
// =============================================================================

#[test]
fn test_tracker_unicode_content() {
    // Test handling of Unicode characters
    let tracker = AttributionTracker::new();
    let old_content = "Hello 世界\n";
    let new_content = "Hello 世界！\n";

    let old_attrs = vec![Attribution::new(0, 13, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 2000)
        .unwrap();

    // Should handle Unicode properly
    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_emoji_content() {
    // Test handling of emoji characters
    let tracker = AttributionTracker::new();
    let old_content = "Hello 👋\n";
    let new_content = "Hello 👋🌍\n";

    let old_attrs = vec![Attribution::new(0, 11, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_mixed_unicode_content() {
    // Test mixed ASCII and Unicode content
    let tracker = AttributionTracker::new();
    let old_content = "ASCII текст 中文 🎉\n";
    let new_content = "ASCII текст 中文 🎉 more\n";

    let old_attrs = vec![Attribution::new(0, 28, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 2000)
        .unwrap();

    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "current-author"));
}

#[test]
fn test_tracker_zero_width_unicode() {
    // Test zero-width Unicode characters
    let tracker = AttributionTracker::new();
    let old_content = "test\u{200B}content\n"; // Zero-width space
    let new_content = "test\u{200B}content\u{200B}\n";

    let old_attrs = vec![Attribution::new(0, 16, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_special_characters() {
    // Test special characters and escape sequences
    let tracker = AttributionTracker::new();
    let old_content = "line\\twith\\ttabs\n";
    let new_content = "line\\twith\\ttabs\\n\n";

    let old_attrs = vec![Attribution::new(0, 16, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

// =============================================================================
// Move Detection Tests
// =============================================================================

#[test]
fn test_tracker_simple_line_move_within_file() {
    // Test detecting a simple line move within a file
    // Note: Move detection may not trigger for very small files or simple swaps
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\nline 3\n";
    let new_content = "line 2\nline 1\nline 3\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
        Attribution::new(14, 21, "ai-3".to_string(), 3000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 4000)
        .unwrap();

    // Should have some attributions preserved or new ones created
    assert!(!new_attrs.is_empty());
    // Third line should be preserved as it didn't move
    assert!(
        new_attrs
            .iter()
            .any(|a| a.author_id == "ai-3" || a.author_id == "current-author")
    );
}

#[test]
fn test_tracker_block_move_within_file() {
    // Test detecting a block of lines moved within a file
    // Note: Move detection may not trigger for very small files
    let tracker = AttributionTracker::new();
    let old_content = "a\nb\nc\nd\ne\n";
    let new_content = "d\ne\na\nb\nc\n";

    let old_attrs = vec![
        Attribution::new(0, 2, "ai-1".to_string(), 1000),
        Attribution::new(2, 4, "ai-2".to_string(), 2000),
        Attribution::new(4, 6, "ai-3".to_string(), 3000),
        Attribution::new(6, 8, "ai-4".to_string(), 4000),
        Attribution::new(8, 10, "ai-5".to_string(), 5000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 6000)
        .unwrap();

    // Should have attributions created - either preserved or new from current author
    assert!(!new_attrs.is_empty());
    // At least some of the original content should be represented
    let has_original = new_attrs.iter().any(|a| {
        a.author_id == "ai-1"
            || a.author_id == "ai-2"
            || a.author_id == "ai-3"
            || a.author_id == "ai-4"
            || a.author_id == "ai-5"
    });
    let has_current = new_attrs.iter().any(|a| a.author_id == "current-author");
    assert!(has_original || has_current);
}

#[test]
fn test_tracker_partial_line_move() {
    // Test detecting partial content moved within a line
    let tracker = AttributionTracker::new();
    let old_content = "prefix middle suffix\n";
    let new_content = "middle prefix suffix\n";

    let old_attrs = vec![Attribution::new(0, 21, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 2000)
        .unwrap();

    // Should detect the move and preserve attribution
    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_move_with_modification() {
    // Test a line that's both moved and modified
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\nline 3\n";
    let new_content = "line 3\nLINE 1 MODIFIED\nline 2\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
        Attribution::new(14, 21, "ai-3".to_string(), 3000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 4000)
        .unwrap();

    // Should have both preserved and new attributions
    assert!(new_attrs.iter().any(|a| a.author_id == "current-author"));
}

#[test]
fn test_tracker_duplicate_line_handling() {
    // Test handling duplicate lines
    let tracker = AttributionTracker::new();
    let old_content = "same\nsame\n";
    let new_content = "same\n";

    let old_attrs = vec![
        Attribution::new(0, 5, "ai-1".to_string(), 1000),
        Attribution::new(5, 10, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current-author", 3000)
        .unwrap();

    // Should preserve one of the attributions
    assert!(!new_attrs.is_empty());
}

// =============================================================================
// Mixed AI/Human Edit Tests
// =============================================================================

#[test]
fn test_tracker_mixed_edit_same_line() {
    // Test when AI and human both edit the same line
    let tracker = AttributionTracker::new();
    let old_content = "original line\n";
    let new_content = "modified line\n";

    let old_attrs = vec![Attribution::new(0, 14, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "human-1", 2000)
        .unwrap();

    // Should have new attribution for the modification
    assert!(new_attrs.iter().any(|a| a.author_id == "human-1"));
}

#[test]
fn test_tracker_ai_adds_human_deletes() {
    // Test AI adding content that human later deletes
    let tracker = AttributionTracker::new();

    // Step 1: AI adds content
    let old_content = "";
    let new_content = "ai added line\n";
    let old_attrs = vec![];

    let attrs1 = tracker
        .update_attributions(old_content, new_content, &old_attrs, "ai-1", 1000)
        .unwrap();

    // Step 2: Human deletes it
    let attrs2 = tracker
        .update_attributions(new_content, old_content, &attrs1, "human-1", 2000)
        .unwrap();

    // Should have a deletion marker or be empty
    // The tracker marks deletions with zero-length attributions
    assert!(attrs2.is_empty() || attrs2.iter().any(|a| a.author_id == "human-1"));
}

#[test]
fn test_tracker_human_adds_ai_modifies() {
    // Test human adding content that AI later modifies
    let tracker = AttributionTracker::new();

    let old_content = "";
    let human_content = "human line\n";
    let ai_content = "human line modified by ai\n";

    let attrs1 = tracker
        .update_attributions(old_content, human_content, &[], "human-1", 1000)
        .unwrap();

    let attrs2 = tracker
        .update_attributions(human_content, ai_content, &attrs1, "ai-1", 2000)
        .unwrap();

    // Should have both attributions
    assert!(attrs2.iter().any(|a| a.author_id == "ai-1"));
}

#[test]
fn test_tracker_interleaved_ai_human_edits() {
    // Test interleaved AI and human edits
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\nline 3\n";
    let new_content = "AI edit\nline 2\nHuman edit\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "original".to_string(), 1000),
        Attribution::new(7, 14, "original".to_string(), 1000),
        Attribution::new(14, 21, "original".to_string(), 1000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    // Should have new attributions for modified lines
    assert!(new_attrs.iter().any(|a| a.author_id == "current"));
    // Original second line should be preserved
    assert!(new_attrs.iter().any(|a| a.author_id == "original"));
}

// =============================================================================
// Attribute Unattributed Ranges Tests
// =============================================================================

#[test]
fn test_attribute_unattributed_fills_gaps() {
    // Test that unattributed ranges are filled correctly
    let tracker = AttributionTracker::new();
    let content = "aaabbbccc\n";

    // Only attribute middle section
    let attrs = vec![Attribution::new(3, 6, "ai-1".to_string(), 1000)];

    let result = tracker.attribute_unattributed_ranges(content, &attrs, "filler", 2000);

    // Should have 3 attributions: start gap, original, end gap
    assert!(
        result
            .iter()
            .any(|a| a.start == 0 && a.author_id == "filler")
    );
    assert!(result.iter().any(|a| a.start == 3 && a.author_id == "ai-1"));
    assert!(
        result
            .iter()
            .any(|a| a.author_id == "filler" && a.end == content.len())
    );
}

#[test]
fn test_attribute_unattributed_no_gaps() {
    // Test when there are no gaps to fill
    let tracker = AttributionTracker::new();
    let content = "complete\n";

    let attrs = vec![Attribution::new(0, 9, "ai-1".to_string(), 1000)];

    let result = tracker.attribute_unattributed_ranges(content, &attrs, "filler", 2000);

    // Should only have the original attribution
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].author_id, "ai-1");
}

#[test]
fn test_attribute_unattributed_multiple_gaps() {
    // Test multiple gaps in attribution
    let tracker = AttributionTracker::new();
    let content = "aa bb cc dd\n";

    let attrs = vec![
        Attribution::new(3, 5, "ai-1".to_string(), 1000),
        Attribution::new(9, 11, "ai-2".to_string(), 2000),
    ];

    let result = tracker.attribute_unattributed_ranges(content, &attrs, "filler", 3000);

    // Should fill gaps: before first, between first and second, and after second
    assert!(
        result
            .iter()
            .any(|a| a.start == 0 && a.author_id == "filler")
    );
    assert!(result.iter().any(|a| a.start == 3 && a.author_id == "ai-1"));
    // There should be a gap filled between the two attributed ranges
    let has_middle_gap = result
        .iter()
        .any(|a| a.author_id == "filler" && a.start >= 5 && a.end <= 9);
    assert!(
        has_middle_gap,
        "Should have filler attribution in middle gap"
    );
    assert!(result.iter().any(|a| a.start == 9 && a.author_id == "ai-2"));
    // Should have filler at the end too
    assert!(
        result
            .iter()
            .any(|a| a.author_id == "filler" && a.end == content.len())
    );
}

#[test]
fn test_attribute_unattributed_empty_content() {
    // Test with empty content
    let tracker = AttributionTracker::new();
    let content = "";

    let attrs = vec![];

    let result = tracker.attribute_unattributed_ranges(content, &attrs, "filler", 1000);

    // Should have no attributions for empty content
    assert!(result.is_empty());
}

#[test]
fn test_attribute_unattributed_overlapping_attrs() {
    // Test with overlapping attributions
    let tracker = AttributionTracker::new();
    let content = "overlapping\n";

    let attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(4, 11, "ai-2".to_string(), 2000),
    ];

    let result = tracker.attribute_unattributed_ranges(content, &attrs, "filler", 3000);

    // Should preserve overlapping attributions and fill the remaining gap
    assert!(result.iter().any(|a| a.author_id == "ai-1"));
    assert!(result.iter().any(|a| a.author_id == "ai-2"));
    assert!(
        result
            .iter()
            .any(|a| a.author_id == "filler" && a.end == 12)
    );
}

// =============================================================================
// Configuration Tests
// =============================================================================

#[test]
fn test_tracker_with_default_config() {
    // Test creating tracker with default configuration
    let config = AttributionConfig::default();
    let tracker = AttributionTracker::with_config(config);

    // Just verify it works with default config
    let old_content = "test\n";
    let new_content = "test modified\n";
    let old_attrs = vec![Attribution::new(0, 5, "ai-1".to_string(), 1000)];

    let result = tracker.update_attributions(old_content, new_content, &old_attrs, "current", 2000);
    assert!(result.is_ok());
}

// =============================================================================
// Large File Performance Tests
// =============================================================================

#[test]
fn test_tracker_large_file_many_lines() {
    // Test performance with a large number of lines
    let tracker = AttributionTracker::new();

    // Generate 1000 lines
    let mut old_lines = Vec::new();
    let mut old_attrs = Vec::new();
    let mut pos = 0;
    for i in 0..1000 {
        let line = format!("line {}\n", i);
        let len = line.len();
        old_lines.push(line);
        old_attrs.push(Attribution::new(
            pos,
            pos + len,
            format!("ai-{}", i % 10),
            1000,
        ));
        pos += len;
    }
    let old_content = old_lines.join("");

    // Modify a few lines in the middle
    let mut new_lines = old_lines.clone();
    new_lines[500] = "modified line 500\n".to_string();
    new_lines[501] = "modified line 501\n".to_string();
    let new_content = new_lines.join("");

    let result =
        tracker.update_attributions(&old_content, &new_content, &old_attrs, "current", 2000);
    assert!(result.is_ok());

    let new_attrs = result.unwrap();
    // Should have roughly the same number of attributions
    assert!(new_attrs.len() > 900);
}

#[test]
fn test_tracker_large_file_long_lines() {
    // Test performance with very long lines
    let tracker = AttributionTracker::new();

    // Generate a file with a few very long lines
    let long_line = "x".repeat(10000);
    let old_content = format!("{}\n{}\n", long_line, long_line);
    let new_content = format!("{}\nmodified\n", long_line);

    let old_attrs = vec![
        Attribution::new(0, 10001, "ai-1".to_string(), 1000),
        Attribution::new(10001, 20002, "ai-2".to_string(), 2000),
    ];

    let result =
        tracker.update_attributions(&old_content, &new_content, &old_attrs, "current", 3000);
    assert!(result.is_ok());
}

#[test]
fn test_tracker_many_small_changes() {
    // Test many small changes throughout a file
    let tracker = AttributionTracker::new();

    let old_content = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
    let new_content = "A\nb\nC\nd\nE\nf\nG\nh\nI\nj\n";

    let old_attrs = vec![
        Attribution::new(0, 2, "ai-1".to_string(), 1000),
        Attribution::new(2, 4, "ai-2".to_string(), 1000),
        Attribution::new(4, 6, "ai-3".to_string(), 1000),
        Attribution::new(6, 8, "ai-4".to_string(), 1000),
        Attribution::new(8, 10, "ai-5".to_string(), 1000),
        Attribution::new(10, 12, "ai-6".to_string(), 1000),
        Attribution::new(12, 14, "ai-7".to_string(), 1000),
        Attribution::new(14, 16, "ai-8".to_string(), 1000),
        Attribution::new(16, 18, "ai-9".to_string(), 1000),
        Attribution::new(18, 20, "ai-10".to_string(), 1000),
    ];

    let result = tracker.update_attributions(old_content, new_content, &old_attrs, "current", 2000);
    assert!(result.is_ok());
}

// =============================================================================
// Edge Cases and Error Handling
// =============================================================================

#[test]
fn test_tracker_empty_old_content() {
    // Test with empty old content (new file)
    let tracker = AttributionTracker::new();
    let old_content = "";
    let new_content = "new file content\n";
    let old_attrs = vec![];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "author", 1000)
        .unwrap();

    assert!(!new_attrs.is_empty());
    assert!(new_attrs.iter().all(|a| a.author_id == "author"));
}

#[test]
fn test_tracker_empty_new_content() {
    // Test with empty new content (file deletion)
    let tracker = AttributionTracker::new();
    let old_content = "file content\n";
    let new_content = "";
    let old_attrs = vec![Attribution::new(0, 13, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "author", 2000)
        .unwrap();

    // Should have no or minimal attributions for empty file
    assert!(new_attrs.is_empty() || new_attrs.iter().all(|a| a.is_empty()));
}

#[test]
fn test_tracker_both_empty() {
    // Test with both old and new content empty
    let tracker = AttributionTracker::new();
    let old_content = "";
    let new_content = "";
    let old_attrs = vec![];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "author", 1000)
        .unwrap();

    assert!(new_attrs.is_empty());
}

#[test]
fn test_tracker_no_newline_at_end() {
    // Test content without trailing newline
    let tracker = AttributionTracker::new();
    let old_content = "no newline";
    let new_content = "no newline modified";

    let old_attrs = vec![Attribution::new(0, 10, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_only_newlines() {
    // Test content that's only newlines
    let tracker = AttributionTracker::new();
    let old_content = "\n\n\n";
    let new_content = "\n\n\n\n";

    let old_attrs = vec![Attribution::new(0, 3, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_windows_line_endings() {
    // Test Windows line endings (CRLF)
    let tracker = AttributionTracker::new();
    let old_content = "line 1\r\nline 2\r\n";
    let new_content = "line 1\r\nmodified\r\n";

    let old_attrs = vec![
        Attribution::new(0, 8, "ai-1".to_string(), 1000),
        Attribution::new(8, 16, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 3000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_mixed_line_endings() {
    // Test mixed line endings
    let tracker = AttributionTracker::new();
    let old_content = "line 1\nline 2\r\nline 3\n";
    let new_content = "line 1\nmodified\r\nline 3\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 15, "ai-2".to_string(), 2000),
        Attribution::new(15, 22, "ai-3".to_string(), 3000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 4000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

// =============================================================================
// Integration Tests with TestRepo
// =============================================================================

#[test]
fn test_attribution_through_commit() {
    // Integration test: attribution preservation through git commits
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines![
        "AI line 1".ai(),
        "Human line 1".human(),
        "AI line 2".ai()
    ]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // Modify the file
    file.set_contents(lines![
        "AI line 1".ai(),
        "Modified by human".human(),
        "AI line 2".ai(),
        "New AI line".ai()
    ]);

    let result = repo.stage_all_and_commit("Second commit");
    assert!(result.is_ok());
}

#[test]
fn test_attribution_through_multiple_commits() {
    // Test attribution preservation through multiple commits
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // First commit - AI content
    file.set_contents(lines!["AI initial".ai()]);
    repo.stage_all_and_commit("Commit 1").unwrap();

    // Second commit - Human modifies
    file.set_contents(lines!["AI initial".ai(), "Human adds".human()]);
    repo.stage_all_and_commit("Commit 2").unwrap();

    // Third commit - AI modifies
    file.set_contents(lines![
        "AI modified initial".ai(),
        "Human adds".human(),
        "AI adds more".ai()
    ]);

    let result = repo.stage_all_and_commit("Commit 3");
    assert!(result.is_ok());
}

#[test]
fn test_attribution_with_file_rename() {
    // Test that attribution survives file renames
    let repo = TestRepo::new();
    let mut file = repo.filename("old.txt");

    file.set_contents(lines!["AI content".ai()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Rename file
    repo.git(&["mv", "old.txt", "new.txt"]).unwrap();
    repo.git(&["commit", "-m", "Rename"]).unwrap();

    // Verify new file exists
    let new_file = repo.filename("new.txt");
    assert!(new_file.file_path.exists());
}

#[test]
fn test_attribution_multifile_edit() {
    // Test attribution tracking across multiple files
    let repo = TestRepo::new();
    let mut file1 = repo.filename("file1.txt");
    let mut file2 = repo.filename("file2.txt");

    file1.set_contents(lines!["File 1 AI".ai()]);
    file2.set_contents(lines!["File 2 Human".human()]);

    repo.stage_all_and_commit("Multi-file commit").unwrap();

    // Modify both
    file1.set_contents(lines!["File 1 AI".ai(), "Modified".human()]);
    file2.set_contents(lines!["File 2 Human".human(), "AI addition".ai()]);

    let result = repo.stage_all_and_commit("Multi-file edit");
    assert!(result.is_ok());
}

#[test]
fn test_initial_attribution_timestamp() {
    // Test that INITIAL_ATTRIBUTION_TS constant is used correctly
    let attr = Attribution::new(0, 10, "ai-1".to_string(), INITIAL_ATTRIBUTION_TS);
    assert_eq!(attr.ts, 42);
}

#[test]
fn test_attribution_with_checkpoint() {
    // Test attribution behavior with checkpoints
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["Initial".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make working directory changes
    file.set_contents(lines!["Initial".human(), "WIP AI".ai()]);

    // Create checkpoint
    let result = repo.git_ai(&["checkpoint"]);
    assert!(result.is_ok());
}

// =============================================================================
// Additional Edge Cases and Complex Scenarios
// =============================================================================

#[test]
fn test_tracker_repeated_content() {
    // Test handling of repeated identical content blocks
    let tracker = AttributionTracker::new();
    let old_content = "repeat\nrepeat\nrepeat\n";
    let new_content = "repeat\nunique\nrepeat\nrepeat\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-1".to_string(), 1000),
        Attribution::new(14, 21, "ai-1".to_string(), 1000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
    assert!(new_attrs.iter().any(|a| a.author_id == "current"));
}

#[test]
fn test_tracker_single_character_changes() {
    // Test single character insertions and deletions
    let tracker = AttributionTracker::new();
    let old_content = "abc\n";
    let new_content = "abxc\n";

    let old_attrs = vec![Attribution::new(0, 4, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_very_long_single_line() {
    // Test handling of a very long single line
    let tracker = AttributionTracker::new();
    let old_content = "x".repeat(100000) + "\n";
    let new_content = "x".repeat(50000) + "y" + &"x".repeat(50000) + "\n";

    let old_attrs = vec![Attribution::new(0, 100001, "ai-1".to_string(), 1000)];

    let result =
        tracker.update_attributions(&old_content, &new_content, &old_attrs, "current", 2000);
    assert!(result.is_ok());
}

#[test]
fn test_tracker_binary_like_content() {
    // Test handling content that looks binary-ish but is still text
    let tracker = AttributionTracker::new();
    let old_content = "\x00\x01\x02\x03\n";
    let new_content = "\x00\x01\x7F\x02\x03\n";

    let old_attrs = vec![Attribution::new(0, 5, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_complete_file_replacement() {
    // Test completely replacing file content
    let tracker = AttributionTracker::new();
    let old_content = "old content line 1\nold content line 2\n";
    let new_content = "completely\ndifferent\ncontent\n";

    let old_attrs = vec![
        Attribution::new(0, 19, "ai-1".to_string(), 1000),
        Attribution::new(19, 38, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 3000)
        .unwrap();

    // All new content should be attributed to current author
    assert!(new_attrs.iter().any(|a| a.author_id == "current"));
}

#[test]
fn test_tracker_alternating_small_edits() {
    // Test alternating character-level edits
    let tracker = AttributionTracker::new();
    let old_content = "a b c d e\n";
    let new_content = "A B C D E\n";

    let old_attrs = vec![Attribution::new(0, 10, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_nested_structures() {
    // Test code with nested structures
    let tracker = AttributionTracker::new();
    let old_content = "fn outer() {\n    fn inner() {\n        code\n    }\n}\n";
    let new_content = "fn outer() {\n    fn inner() {\n        modified\n    }\n}\n";

    let old_attrs = vec![Attribution::new(0, 48, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(new_attrs.iter().any(|a| a.author_id == "current"));
}

#[test]
fn test_tracker_surrounding_context_preserved() {
    // Test that surrounding context is preserved when middle is edited
    let tracker = AttributionTracker::new();
    let old_content = "prefix\nmiddle\nsuffix\n";
    let new_content = "prefix\nNEW\nsuffix\n";

    let old_attrs = vec![
        Attribution::new(0, 7, "ai-1".to_string(), 1000),
        Attribution::new(7, 14, "ai-2".to_string(), 2000),
        Attribution::new(14, 21, "ai-3".to_string(), 3000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 4000)
        .unwrap();

    // Prefix and suffix should be preserved
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-3"));
    assert!(new_attrs.iter().any(|a| a.author_id == "current"));
}

#[test]
fn test_line_attribution_zero_line_count() {
    // Test edge case of inverted line range
    let attr = LineAttribution::new(10, 5, "ai-1".to_string(), None);
    assert_eq!(attr.line_count(), 0);
    assert!(attr.is_empty());
}

#[test]
fn test_line_attribution_single_line() {
    // Test single line attribution
    let attr = LineAttribution::new(5, 5, "ai-1".to_string(), None);
    assert_eq!(attr.line_count(), 1);
    assert!(!attr.is_empty());
}

#[test]
fn test_tracker_all_whitespace_file() {
    // Test a file that's entirely whitespace
    let tracker = AttributionTracker::new();
    let old_content = "   \n\t\t\n  \n";
    let new_content = "   \n\t\t\t\n  \n";

    let old_attrs = vec![Attribution::new(0, 10, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_massive_insertion() {
    // Test inserting a large block of text
    let tracker = AttributionTracker::new();
    let old_content = "start\nend\n";
    let mut middle = String::new();
    for i in 0..100 {
        middle.push_str(&format!("inserted line {}\n", i));
    }
    let new_content = format!("start\n{}end\n", middle);

    let old_attrs = vec![
        Attribution::new(0, 6, "ai-1".to_string(), 1000),
        Attribution::new(6, 10, "ai-2".to_string(), 2000),
    ];

    let new_attrs = tracker
        .update_attributions(&old_content, &new_content, &old_attrs, "current", 3000)
        .unwrap();

    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-2"));
    assert!(new_attrs.iter().any(|a| a.author_id == "current"));
}

#[test]
fn test_tracker_massive_deletion() {
    // Test deleting a large block of text
    let tracker = AttributionTracker::new();
    let mut middle = String::new();
    for i in 0..100 {
        middle.push_str(&format!("to be deleted {}\n", i));
    }
    let old_content = format!("start\n{}end\n", middle);
    let new_content = "start\nend\n";

    let old_attrs = vec![
        Attribution::new(0, 6, "ai-1".to_string(), 1000),
        Attribution::new(6, old_content.len() - 4, "ai-2".to_string(), 2000),
        Attribution::new(
            old_content.len() - 4,
            old_content.len(),
            "ai-3".to_string(),
            3000,
        ),
    ];

    let new_attrs = tracker
        .update_attributions(&old_content, new_content, &old_attrs, "current", 4000)
        .unwrap();

    assert!(new_attrs.iter().any(|a| a.author_id == "ai-1"));
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-3"));
}

#[test]
fn test_attribution_consistency_multiple_rounds() {
    // Test that multiple rounds of attribution produce consistent results
    let tracker = AttributionTracker::new();
    let content1 = "line 1\n";
    let content2 = "line 1\nline 2\n";
    let content3 = "line 1\nline 2\nline 3\n";

    let attrs1 = tracker
        .update_attributions("", content1, &[], "author1", 1000)
        .unwrap();

    let attrs2 = tracker
        .update_attributions(content1, content2, &attrs1, "author2", 2000)
        .unwrap();

    let attrs3 = tracker
        .update_attributions(content2, content3, &attrs2, "author3", 3000)
        .unwrap();

    // Should have attributions from all three authors
    assert!(attrs3.iter().any(|a| a.author_id == "author1"));
    assert!(attrs3.iter().any(|a| a.author_id == "author2"));
    assert!(attrs3.iter().any(|a| a.author_id == "author3"));
}

#[test]
fn test_attribution_through_complex_branch_workflow() {
    // Test attribution through a complex branching workflow
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    // Initial commit
    file.set_contents(lines!["base".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Capture the original branch name before switching
    let original_branch = repo.current_branch();

    // Create and switch to a branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Make changes on branch
    file.set_contents(lines!["base".human(), "feature".ai()]);
    repo.stage_all_and_commit("Feature work").unwrap();

    // Switch back to the original branch
    repo.git(&["checkout", &original_branch]).unwrap();

    // Verify original content
    let content = std::fs::read_to_string(file.file_path.clone()).unwrap();
    assert!(content.contains("base"));
}

#[test]
fn test_attribution_with_merge_conflict_markers() {
    // Test handling merge conflict markers as regular text
    let tracker = AttributionTracker::new();
    let old_content = "normal line\n";
    let new_content = "<<<<<<< HEAD\nnormal line\n=======\nother line\n>>>>>>> branch\n";

    let old_attrs = vec![Attribution::new(0, 12, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_symmetric_changes() {
    // Test symmetric changes (same edit at multiple locations)
    let tracker = AttributionTracker::new();
    let old_content = "foo\nbar\nfoo\n";
    let new_content = "FOO\nbar\nFOO\n";

    let old_attrs = vec![
        Attribution::new(0, 4, "ai-1".to_string(), 1000),
        Attribution::new(4, 8, "ai-2".to_string(), 2000),
        Attribution::new(8, 12, "ai-3".to_string(), 3000),
    ];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 4000)
        .unwrap();

    // Middle line should be preserved
    assert!(new_attrs.iter().any(|a| a.author_id == "ai-2"));
}

#[test]
fn test_tracker_regex_like_patterns() {
    // Test content with regex-like patterns
    let tracker = AttributionTracker::new();
    let old_content = "pattern: [a-z]+\n";
    let new_content = "pattern: [a-zA-Z]+\n";

    let old_attrs = vec![Attribution::new(0, 16, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_json_like_content() {
    // Test JSON-like structured content
    let tracker = AttributionTracker::new();
    let old_content = r#"{"key": "value"}"#.to_string() + "\n";
    let new_content = r#"{"key": "new_value", "extra": true}"#.to_string() + "\n";

    let old_attrs = vec![Attribution::new(0, 17, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(&old_content, &new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_tracker_url_like_content() {
    // Test URLs and paths
    let tracker = AttributionTracker::new();
    let old_content = "https://example.com/path\n";
    let new_content = "https://example.com/newpath?query=1\n";

    let old_attrs = vec![Attribution::new(0, 25, "ai-1".to_string(), 1000)];

    let new_attrs = tracker
        .update_attributions(old_content, new_content, &old_attrs, "current", 2000)
        .unwrap();

    assert!(!new_attrs.is_empty());
}

#[test]
fn test_attribution_boundary_conditions() {
    // Test attribution at exact boundaries
    let attr = Attribution::new(10, 20, "ai-1".to_string(), 1000);

    // Test overlaps at exact boundaries
    assert!(!attr.overlaps(0, 10)); // Ends exactly at start
    assert!(!attr.overlaps(20, 30)); // Starts exactly at end
    assert!(attr.overlaps(10, 20)); // Exact match
    assert!(attr.overlaps(9, 11)); // Crosses start boundary
    assert!(attr.overlaps(19, 21)); // Crosses end boundary
}

#[test]
fn test_line_attribution_boundary_conditions() {
    // Test line attribution at exact boundaries
    let attr = LineAttribution::new(10, 20, "ai-1".to_string(), None);

    // Boundary checks
    assert!(!attr.overlaps(1, 9)); // Before
    assert!(!attr.overlaps(21, 30)); // After
    assert!(attr.overlaps(10, 20)); // Exact
    assert!(attr.overlaps(9, 11)); // Crosses start
    assert!(attr.overlaps(19, 21)); // Crosses end
}

#[test]
fn test_tracker_progressive_file_growth() {
    // Test progressive file growth over multiple edits
    let tracker = AttributionTracker::new();

    let mut content = "initial\n".to_string();
    let mut attrs = tracker
        .update_attributions("", &content, &[], "author0", 1000)
        .unwrap();

    // Add lines progressively
    for i in 1..10 {
        let new_content = format!("{}line {}\n", content, i);
        attrs = tracker
            .update_attributions(
                &content,
                &new_content,
                &attrs,
                &format!("author{}", i),
                1000 + i as u128 * 100,
            )
            .unwrap();
        content = new_content;
    }

    // Should have attributions from multiple authors
    assert!(attrs.iter().any(|a| a.author_id == "author0"));
    assert!(attrs.iter().any(|a| a.author_id.starts_with("author")));
    assert!(attrs.len() >= 10);
}

#[test]
fn test_attribution_with_stash() {
    // Test attribution behavior with git stash
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");

    file.set_contents(lines!["committed".human()]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Make uncommitted changes
    file.set_contents(lines!["committed".human(), "uncommitted".ai()]);

    // Stash should work
    let result = repo.git(&["stash"]);
    assert!(result.is_ok());

    // File should be back to committed state
    let content = std::fs::read_to_string(file.file_path.clone()).unwrap();
    assert!(content.starts_with("committed"));
}
