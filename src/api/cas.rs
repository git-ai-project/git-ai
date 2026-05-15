use sha2::{Digest, Sha256};

use crate::api::client::HttpClient;

/// Content-addressable storage uploader for authorship notes.
pub struct CasUploader;

impl CasUploader {
    /// Compute the SHA-256 CAS key for the given content.
    pub fn compute_hash(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Upload an authorship note to CAS.
    ///
    /// Returns the CAS hash on success.
    /// If the server returns 409 (already exists), this is treated as success
    /// since the content is already stored.
    pub fn upload_note(
        client: &HttpClient,
        _commit_sha: &str,
        note_content: &str,
    ) -> Result<String, String> {
        let hash = Self::compute_hash(note_content);
        let path = format!("/v1/cas/{hash}");

        let response = client.post_raw(&path, note_content)?;

        match response.status {
            200..=299 | 409 => Ok(hash), // 409 = already exists (dedup success)
            other => Err(format!(
                "CAS upload failed: HTTP {other}: {}",
                response.body
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hash_deterministic() {
        let content = "authorship/3.0.0\nsome note content";
        let hash1 = CasUploader::compute_hash(content);
        let hash2 = CasUploader::compute_hash(content);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_content() {
        let hash1 = CasUploader::compute_hash("content A");
        let hash2 = CasUploader::compute_hash("content B");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_is_sha256() {
        // Known SHA-256 of empty string
        let hash = CasUploader::compute_hash("");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_compute_hash_length() {
        let hash = CasUploader::compute_hash("hello world");
        // SHA-256 hex is always 64 characters
        assert_eq!(hash.len(), 64);
    }
}
