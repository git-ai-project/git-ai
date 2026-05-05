//! Watermarking strategies for tracking transcript processing progress.

use super::types::TranscriptError;
use chrono::{DateTime, Utc};
use std::fmt;
use std::str::FromStr;

/// Strategy for tracking progress through a transcript.
pub trait WatermarkStrategy: Send + Sync {
    /// Serialize the watermark to a string for database storage.
    fn serialize(&self) -> String;

    /// Advance the watermark based on bytes and records read.
    fn advance(&mut self, bytes_read: usize, records_read: usize);

    /// Downcast support for concrete watermark types.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Type of watermark strategy (used for deserialization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatermarkType {
    ByteOffset,
    RecordIndex,
    Timestamp,
    Hybrid,
}

impl fmt::Display for WatermarkType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatermarkType::ByteOffset => write!(f, "ByteOffset"),
            WatermarkType::RecordIndex => write!(f, "RecordIndex"),
            WatermarkType::Timestamp => write!(f, "Timestamp"),
            WatermarkType::Hybrid => write!(f, "Hybrid"),
        }
    }
}

impl FromStr for WatermarkType {
    type Err = TranscriptError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ByteOffset" => Ok(WatermarkType::ByteOffset),
            "RecordIndex" => Ok(WatermarkType::RecordIndex),
            "Timestamp" => Ok(WatermarkType::Timestamp),
            "Hybrid" => Ok(WatermarkType::Hybrid),
            _ => Err(TranscriptError::Parse {
                line: 0,
                message: format!("Invalid watermark type: {}", s),
            }),
        }
    }
}

impl WatermarkType {
    /// Deserialize a watermark value based on the strategy type.
    pub fn deserialize(&self, s: &str) -> Result<Box<dyn WatermarkStrategy>, TranscriptError> {
        match self {
            WatermarkType::ByteOffset => Ok(Box::new(ByteOffsetWatermark::from_str(s)?)),
            WatermarkType::RecordIndex => Ok(Box::new(RecordIndexWatermark::from_str(s)?)),
            WatermarkType::Timestamp => Ok(Box::new(TimestampWatermark::from_str(s)?)),
            WatermarkType::Hybrid => Ok(Box::new(HybridWatermark::from_str(s)?)),
        }
    }
}

/// Byte-offset based watermark for append-only files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteOffsetWatermark(pub u64);

impl ByteOffsetWatermark {
    pub fn new(offset: u64) -> Self {
        Self(offset)
    }
}

impl WatermarkStrategy for ByteOffsetWatermark {
    fn serialize(&self) -> String {
        self.0.to_string()
    }

    fn advance(&mut self, bytes_read: usize, _records_read: usize) {
        self.0 += bytes_read as u64;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl FromStr for ByteOffsetWatermark {
    type Err = TranscriptError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u64>()
            .map(ByteOffsetWatermark)
            .map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid byte offset watermark: {}", e),
            })
    }
}

/// Record-index based watermark for sequential formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordIndexWatermark(pub u64);

impl RecordIndexWatermark {
    pub fn new(index: u64) -> Self {
        Self(index)
    }
}

impl WatermarkStrategy for RecordIndexWatermark {
    fn serialize(&self) -> String {
        self.0.to_string()
    }

    fn advance(&mut self, _bytes_read: usize, records_read: usize) {
        self.0 += records_read as u64;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl FromStr for RecordIndexWatermark {
    type Err = TranscriptError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u64>()
            .map(RecordIndexWatermark)
            .map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid record index watermark: {}", e),
            })
    }
}

/// Timestamp-based watermark for time-ordered streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampWatermark(pub DateTime<Utc>);

impl TimestampWatermark {
    pub fn new(timestamp: DateTime<Utc>) -> Self {
        Self(timestamp)
    }
}

impl WatermarkStrategy for TimestampWatermark {
    fn serialize(&self) -> String {
        self.0.to_rfc3339()
    }

    fn advance(&mut self, _bytes_read: usize, _records_read: usize) {
        // Timestamp watermarks don't auto-advance
        // They must be explicitly updated based on record timestamps
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl FromStr for TimestampWatermark {
    type Err = TranscriptError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| TimestampWatermark(dt.with_timezone(&Utc)))
            .map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid timestamp watermark: {}", e),
            })
    }
}

/// Hybrid watermark combining multiple strategies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HybridWatermark {
    pub offset: u64,
    pub record: u64,
    pub timestamp: Option<DateTime<Utc>>,
}

impl HybridWatermark {
    pub fn new(offset: u64, record: u64, timestamp: Option<DateTime<Utc>>) -> Self {
        Self {
            offset,
            record,
            timestamp,
        }
    }
}

impl WatermarkStrategy for HybridWatermark {
    fn serialize(&self) -> String {
        match &self.timestamp {
            Some(ts) => format!("{}|{}|{}", self.offset, self.record, ts.to_rfc3339()),
            None => format!("{}|{}|", self.offset, self.record),
        }
    }

    fn advance(&mut self, bytes_read: usize, records_read: usize) {
        self.offset += bytes_read as u64;
        self.record += records_read as u64;
        // Timestamp must be explicitly updated based on record data
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl FromStr for HybridWatermark {
    type Err = TranscriptError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('|').collect();
        if parts.len() != 3 {
            return Err(TranscriptError::Parse {
                line: 0,
                message: format!(
                    "Invalid hybrid watermark format: expected 3 parts, got {}",
                    parts.len()
                ),
            });
        }

        let offset = parts[0]
            .parse::<u64>()
            .map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid offset in hybrid watermark: {}", e),
            })?;

        let record = parts[1]
            .parse::<u64>()
            .map_err(|e| TranscriptError::Parse {
                line: 0,
                message: format!("Invalid record in hybrid watermark: {}", e),
            })?;

        let timestamp = if parts[2].is_empty() {
            None
        } else {
            Some(
                DateTime::parse_from_rfc3339(parts[2])
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|e| TranscriptError::Parse {
                        line: 0,
                        message: format!("Invalid timestamp in hybrid watermark: {}", e),
                    })?,
            )
        };

        Ok(HybridWatermark {
            offset,
            record,
            timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_offset_watermark_serialize() {
        let wm = ByteOffsetWatermark::new(1234);
        assert_eq!(wm.serialize(), "1234");
    }

    #[test]
    fn test_byte_offset_watermark_deserialize() {
        let wm = ByteOffsetWatermark::from_str("5678").unwrap();
        assert_eq!(wm.0, 5678);
    }

    #[test]
    fn test_byte_offset_watermark_advance() {
        let mut wm = ByteOffsetWatermark::new(100);
        wm.advance(50, 10);
        assert_eq!(wm.0, 150);
    }

    #[test]
    fn test_byte_offset_watermark_roundtrip() {
        let original = ByteOffsetWatermark::new(9999);
        let serialized = original.serialize();
        let deserialized = ByteOffsetWatermark::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_byte_offset_watermark_invalid() {
        let result = ByteOffsetWatermark::from_str("not_a_number");
        assert!(result.is_err());
    }

    #[test]
    fn test_record_index_watermark_serialize() {
        let wm = RecordIndexWatermark::new(42);
        assert_eq!(wm.serialize(), "42");
    }

    #[test]
    fn test_record_index_watermark_deserialize() {
        let wm = RecordIndexWatermark::from_str("123").unwrap();
        assert_eq!(wm.0, 123);
    }

    #[test]
    fn test_record_index_watermark_advance() {
        let mut wm = RecordIndexWatermark::new(10);
        wm.advance(1000, 5);
        assert_eq!(wm.0, 15);
    }

    #[test]
    fn test_record_index_watermark_roundtrip() {
        let original = RecordIndexWatermark::new(7777);
        let serialized = original.serialize();
        let deserialized = RecordIndexWatermark::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_timestamp_watermark_serialize() {
        let ts = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let wm = TimestampWatermark::new(ts);
        assert_eq!(wm.serialize(), "2024-01-01T12:00:00+00:00");
    }

    #[test]
    fn test_timestamp_watermark_deserialize() {
        let wm = TimestampWatermark::from_str("2024-01-01T12:00:00Z").unwrap();
        let expected = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(wm.0, expected);
    }

    #[test]
    fn test_timestamp_watermark_advance_noop() {
        let ts = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut wm = TimestampWatermark::new(ts);
        let original_ts = wm.0;
        wm.advance(100, 10);
        assert_eq!(wm.0, original_ts); // Should not change
    }

    #[test]
    fn test_timestamp_watermark_roundtrip() {
        let ts = DateTime::parse_from_rfc3339("2024-06-15T08:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let original = TimestampWatermark::new(ts);
        let serialized = original.serialize();
        let deserialized = TimestampWatermark::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_hybrid_watermark_serialize_with_timestamp() {
        let ts = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let wm = HybridWatermark::new(1000, 50, Some(ts));
        assert_eq!(wm.serialize(), "1000|50|2024-01-01T12:00:00+00:00");
    }

    #[test]
    fn test_hybrid_watermark_serialize_without_timestamp() {
        let wm = HybridWatermark::new(2000, 100, None);
        assert_eq!(wm.serialize(), "2000|100|");
    }

    #[test]
    fn test_hybrid_watermark_deserialize_with_timestamp() {
        let wm = HybridWatermark::from_str("1500|75|2024-01-01T12:00:00Z").unwrap();
        assert_eq!(wm.offset, 1500);
        assert_eq!(wm.record, 75);
        assert!(wm.timestamp.is_some());
    }

    #[test]
    fn test_hybrid_watermark_deserialize_without_timestamp() {
        let wm = HybridWatermark::from_str("3000|150|").unwrap();
        assert_eq!(wm.offset, 3000);
        assert_eq!(wm.record, 150);
        assert!(wm.timestamp.is_none());
    }

    #[test]
    fn test_hybrid_watermark_advance() {
        let mut wm = HybridWatermark::new(100, 10, None);
        wm.advance(50, 5);
        assert_eq!(wm.offset, 150);
        assert_eq!(wm.record, 15);
    }

    #[test]
    fn test_hybrid_watermark_roundtrip_with_timestamp() {
        let ts = DateTime::parse_from_rfc3339("2024-03-15T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let original = HybridWatermark::new(5000, 250, Some(ts));
        let serialized = original.serialize();
        let deserialized = HybridWatermark::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_hybrid_watermark_roundtrip_without_timestamp() {
        let original = HybridWatermark::new(6000, 300, None);
        let serialized = original.serialize();
        let deserialized = HybridWatermark::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_hybrid_watermark_invalid_format() {
        let result = HybridWatermark::from_str("1000|50");
        assert!(result.is_err());
    }

    #[test]
    fn test_hybrid_watermark_invalid_offset() {
        let result = HybridWatermark::from_str("abc|50|");
        assert!(result.is_err());
    }

    #[test]
    fn test_hybrid_watermark_invalid_record() {
        let result = HybridWatermark::from_str("1000|xyz|");
        assert!(result.is_err());
    }

    #[test]
    fn test_watermark_type_deserialize_byte_offset() {
        let wm = WatermarkType::ByteOffset.deserialize("1234").unwrap();
        assert_eq!(wm.serialize(), "1234");
    }

    #[test]
    fn test_watermark_type_deserialize_record_index() {
        let wm = WatermarkType::RecordIndex.deserialize("42").unwrap();
        assert_eq!(wm.serialize(), "42");
    }

    #[test]
    fn test_watermark_type_deserialize_timestamp() {
        let wm = WatermarkType::Timestamp
            .deserialize("2024-01-01T12:00:00Z")
            .unwrap();
        assert_eq!(wm.serialize(), "2024-01-01T12:00:00+00:00");
    }

    #[test]
    fn test_watermark_type_deserialize_hybrid() {
        let wm = WatermarkType::Hybrid.deserialize("1000|50|").unwrap();
        assert_eq!(wm.serialize(), "1000|50|");
    }

    #[test]
    fn test_watermark_type_deserialize_invalid() {
        let result = WatermarkType::ByteOffset.deserialize("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_watermark_type_display() {
        assert_eq!(WatermarkType::ByteOffset.to_string(), "ByteOffset");
        assert_eq!(WatermarkType::RecordIndex.to_string(), "RecordIndex");
        assert_eq!(WatermarkType::Timestamp.to_string(), "Timestamp");
        assert_eq!(WatermarkType::Hybrid.to_string(), "Hybrid");
    }

    #[test]
    fn test_watermark_type_from_str() {
        assert_eq!(
            WatermarkType::from_str("ByteOffset").unwrap(),
            WatermarkType::ByteOffset
        );
        assert_eq!(
            WatermarkType::from_str("RecordIndex").unwrap(),
            WatermarkType::RecordIndex
        );
        assert_eq!(
            WatermarkType::from_str("Timestamp").unwrap(),
            WatermarkType::Timestamp
        );
        assert_eq!(
            WatermarkType::from_str("Hybrid").unwrap(),
            WatermarkType::Hybrid
        );
    }

    #[test]
    fn test_watermark_type_from_str_invalid() {
        let result = WatermarkType::from_str("Invalid");
        assert!(result.is_err());
        match result {
            Err(TranscriptError::Parse { message, .. }) => {
                assert!(message.contains("Invalid watermark type"));
            }
            _ => panic!("Expected Parse error"),
        }
    }

    #[test]
    fn test_watermark_type_roundtrip() {
        let types = [
            WatermarkType::ByteOffset,
            WatermarkType::RecordIndex,
            WatermarkType::Timestamp,
            WatermarkType::Hybrid,
        ];

        for wm_type in &types {
            let serialized = wm_type.to_string();
            let deserialized = WatermarkType::from_str(&serialized).unwrap();
            assert_eq!(*wm_type, deserialized);
        }
    }
}
