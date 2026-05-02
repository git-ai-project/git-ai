/// Fast object reading without subprocess spawns
///
/// Handles loose objects only. Packfiles are too complex - fallback to git CLI.

use crate::error::GitAiError;
use flate2::read::ZlibDecoder;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct FastObjectReader {
    git_dir: PathBuf,
}

impl FastObjectReader {
    pub fn new(git_dir: &Path) -> Self {
        Self {
            git_dir: git_dir.to_path_buf(),
        }
    }

    /// Read a loose blob object
    ///
    /// Git loose object format:
    /// - Stored in .git/objects/<first 2 hex>/<remaining 38 hex>
    /// - Zlib compressed
    /// - Content format: "<type> <size>\0<data>"
    ///
    /// Returns:
    /// - Some(data) if loose object exists and is a blob
    /// - None if object is packed or doesn't exist (fallback to git CLI)
    pub fn read_loose_blob(&self, sha: &str) -> Result<Option<Vec<u8>>, GitAiError> {
        // Validate SHA format
        if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(None);
        }

        // Build object path: .git/objects/ab/cdef123...
        let obj_path = self
            .git_dir
            .join("objects")
            .join(&sha[..2])
            .join(&sha[2..]);

        // Read compressed file
        let compressed = match fs::read(&obj_path) {
            Ok(data) => data,
            Err(_) => return Ok(None), // Object doesn't exist or can't be read
        };

        // Decompress with zlib
        let mut decoder = ZlibDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        if decoder.read_to_end(&mut decompressed).is_err() {
            return Ok(None); // Decompression failed
        }

        // Parse object header: "blob 123\0<content>"
        let null_pos = match decompressed.iter().position(|&b| b == 0) {
            Some(pos) => pos,
            None => return Ok(None), // Malformed object
        };

        let header = &decompressed[..null_pos];
        let header_str = match std::str::from_utf8(header) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };

        // Verify it's a blob
        if !header_str.starts_with("blob ") {
            return Ok(None); // Not a blob (could be tree/commit/tag)
        }

        // Extract content after null byte
        let content = decompressed[null_pos + 1..].to_vec();
        Ok(Some(content))
    }

    /// Read a loose commit object
    ///
    /// Similar to read_loose_blob but for commit objects.
    pub fn read_loose_commit(&self, sha: &str) -> Result<Option<Vec<u8>>, GitAiError> {
        // Validate SHA format
        if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(None);
        }

        let obj_path = self
            .git_dir
            .join("objects")
            .join(&sha[..2])
            .join(&sha[2..]);

        let compressed = match fs::read(&obj_path) {
            Ok(data) => data,
            Err(_) => return Ok(None),
        };

        let mut decoder = ZlibDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        if decoder.read_to_end(&mut decompressed).is_err() {
            return Ok(None);
        }

        let null_pos = match decompressed.iter().position(|&b| b == 0) {
            Some(pos) => pos,
            None => return Ok(None),
        };

        let header = &decompressed[..null_pos];
        let header_str = match std::str::from_utf8(header) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };

        // Verify it's a commit
        if !header_str.starts_with("commit ") {
            return Ok(None);
        }

        let content = decompressed[null_pos + 1..].to_vec();
        Ok(Some(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_test_git_dir() -> TempDir {
        let temp = TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(git_dir.join("objects")).unwrap();
        temp
    }

    fn write_loose_object(git_dir: &Path, sha: &str, obj_type: &str, content: &[u8]) {
        let obj_dir = git_dir.join("objects").join(&sha[..2]);
        fs::create_dir_all(&obj_dir).unwrap();

        // Create git object format: "<type> <size>\0<content>"
        let header = format!("{} {}\0", obj_type, content.len());
        let mut full_content = header.as_bytes().to_vec();
        full_content.extend_from_slice(content);

        // Compress with zlib
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&full_content).unwrap();
        let compressed = encoder.finish().unwrap();

        // Write to object file
        let obj_path = obj_dir.join(&sha[2..]);
        fs::write(obj_path, compressed).unwrap();
    }

    #[test]
    fn test_read_loose_blob() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let sha = "abc123def456789012345678901234567890abcd";
        let content = b"Hello, World!";

        write_loose_object(&git_dir, sha, "blob", content);

        let reader = FastObjectReader::new(&git_dir);
        let result = reader.read_loose_blob(sha).unwrap();

        assert_eq!(result, Some(content.to_vec()));
    }

    #[test]
    fn test_read_nonexistent_blob() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let sha = "abc123def456789012345678901234567890abcd";

        let reader = FastObjectReader::new(&git_dir);
        let result = reader.read_loose_blob(sha).unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn test_read_commit_as_blob_returns_none() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let sha = "abc123def456789012345678901234567890abcd";
        let content = b"tree abc\nauthor Test <test@example.com>\n";

        write_loose_object(&git_dir, sha, "commit", content);

        let reader = FastObjectReader::new(&git_dir);
        // Trying to read as blob should return None
        let result = reader.read_loose_blob(sha).unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn test_read_loose_commit() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let sha = "abc123def456789012345678901234567890abcd";
        let content = b"tree abc\nauthor Test <test@example.com>\n";

        write_loose_object(&git_dir, sha, "commit", content);

        let reader = FastObjectReader::new(&git_dir);
        let result = reader.read_loose_commit(sha).unwrap();

        assert_eq!(result, Some(content.to_vec()));
    }

    #[test]
    fn test_invalid_sha_returns_none() {
        let temp = setup_test_git_dir();
        let git_dir = temp.path().join(".git");

        let reader = FastObjectReader::new(&git_dir);

        // Too short
        assert_eq!(reader.read_loose_blob("abc123").unwrap(), None);

        // Invalid characters
        assert_eq!(
            reader
                .read_loose_blob("gggggggggggggggggggggggggggggggggggggggg")
                .unwrap(),
            None
        );
    }
}
