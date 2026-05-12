/// Positional information about a start and end point in the text.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Range {
  /// Start position of the node in the text.
  pub start: usize,
  /// End position of the node in the text.
  pub end: usize,
}

impl Range {
  pub fn new(start: usize, end: usize) -> Self {
    Range { start, end }
  }

  pub fn from_byte_index(pos: usize) -> Self {
    Range::new(pos, pos)
  }
}

impl Ranged for Range {
  fn range(&self) -> Range {
    *self
  }
}

/// Represents an object that has a range in the text.
pub trait Ranged {
  /// Gets the range.
  fn range(&self) -> Range;

  /// Gets the byte index of the first character in the text.
  fn start(&self) -> usize {
    self.range().start
  }

  /// Gets the byte index after the last character in the text.
  fn end(&self) -> usize {
    self.range().end
  }

  /// Gets the text from the provided string.
  fn text<'a>(&self, text: &'a str) -> &'a str {
    let range = self.range();
    &text[range.start..range.end]
  }

  /// Gets the end byte index minus the start byte index of the range.
  fn width(&self) -> usize {
    let range = self.range();
    range.end - range.start
  }
}
