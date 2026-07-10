use flate2::read::ZlibDecoder;
use std::fs;
use std::io::Read;
use std::path::Path;

const MAX_REF_FILE_BYTES: u64 = 4 * 1_024;
// Above these fast-path limits callers fall back to a Git plumbing command.
// Keep them low because repository actors can resolve different repos concurrently.
const MAX_PACKED_REFS_BYTES: u64 = 1_024 * 1_024;
const MAX_LOOSE_OBJECT_COMPRESSED_BYTES: u64 = 8 * 1_024 * 1_024;
const MAX_LOOSE_OBJECT_DECOMPRESSED_BYTES: u64 = 4 * 1_024 * 1_024;
const MAX_LOOSE_OBJECT_HEADER_BYTES: u64 = 4 * 1_024;

fn is_valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn oid_byte_len(oid: &str) -> usize {
    oid.len() / 2
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let byte_len = file.metadata().ok()?.len();
    if byte_len > max_bytes {
        return None;
    }
    let mut bytes = Vec::with_capacity(byte_len as usize);
    file.take(max_bytes + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max_bytes).then_some(bytes)
}

fn read_bounded_text_file(path: &Path, max_bytes: u64) -> Option<String> {
    String::from_utf8(read_bounded_file(path, max_bytes)?).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadKind {
    Symbolic(String),
    Detached(String),
}

/// Fast worktree-aware ref resolution by reading .git/ files directly.
///
/// Handles loose refs, packed-refs, and symbolic refs (one level of indirection).
/// Returns None when the fast path cannot resolve (caller falls back to git CLI).
pub struct FastRefReader<'a> {
    git_dir: &'a Path,
    common_dir: &'a Path,
}

impl<'a> FastRefReader<'a> {
    pub fn new(git_dir: &'a Path, common_dir: &'a Path) -> Self {
        Self {
            git_dir,
            common_dir,
        }
    }

    /// Read HEAD and determine if it's symbolic or detached.
    ///
    /// Returns `Some(Symbolic(...))` for symbolic HEAD, `Some(Detached(...))` for
    /// detached HEAD, or `None` if HEAD can't be read or has unexpected format.
    pub fn try_read_head(&self) -> Option<HeadKind> {
        let head_path = self.git_dir.join("HEAD");
        let content = read_bounded_text_file(&head_path, MAX_REF_FILE_BYTES)?;
        let trimmed = content.trim();

        if let Some(refname) = trimmed.strip_prefix("ref: ") {
            let refname = refname.trim();
            if !refname.is_empty() {
                return Some(HeadKind::Symbolic(refname.to_string()));
            }
        }

        if is_valid_git_oid(trimmed) {
            return Some(HeadKind::Detached(trimmed.to_string()));
        }

        None
    }

    /// Resolve a refname (e.g., "refs/heads/main") to its OID.
    ///
    /// Checks loose refs in both common_dir and git_dir, then packed-refs.
    /// Handles one level of symbolic ref indirection.
    /// Returns None if the ref cannot be resolved via filesystem.
    pub fn try_resolve_ref(&self, refname: &str) -> Option<String> {
        if refname == "HEAD" {
            match self.try_read_head()? {
                HeadKind::Detached(oid) => return Some(oid),
                HeadKind::Symbolic(target) => return self.try_resolve_ref(&target),
            }
        }

        // Check loose refs: common_dir first, then git_dir
        for base in [self.common_dir, self.git_dir] {
            let path = base.join(refname);
            if let Some(contents) = read_bounded_text_file(&path, MAX_REF_FILE_BYTES) {
                let candidate = contents.trim();
                if is_valid_git_oid(candidate) {
                    return Some(candidate.to_string());
                }
                // One level of symbolic ref indirection
                if let Some(target) = candidate.strip_prefix("ref: ") {
                    let target = target.trim();
                    return self.resolve_without_recursion(target);
                }
            }
        }

        // Check packed-refs in common_dir
        self.try_packed_ref(refname)
    }

    fn resolve_without_recursion(&self, refname: &str) -> Option<String> {
        for base in [self.common_dir, self.git_dir] {
            let path = base.join(refname);
            if let Some(contents) = read_bounded_text_file(&path, MAX_REF_FILE_BYTES) {
                let candidate = contents.trim();
                if is_valid_git_oid(candidate) {
                    return Some(candidate.to_string());
                }
            }
        }
        self.try_packed_ref(refname)
    }

    fn try_packed_ref(&self, refname: &str) -> Option<String> {
        let packed_refs_path = self.common_dir.join("packed-refs");
        let contents = read_bounded_text_file(&packed_refs_path, MAX_PACKED_REFS_BYTES)?;

        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let oid = parts.next()?;
            let name = parts.next()?;
            if name == refname && is_valid_git_oid(oid) {
                return Some(oid.to_string());
            }
        }
        None
    }
}

/// Fast loose object reading by directly parsing .git/objects/ files.
///
/// Only handles loose objects (not packfiles). Returns None for packed objects,
/// allowing the caller to fall back to git CLI.
pub struct FastObjectReader<'a> {
    common_dir: &'a Path,
}

impl<'a> FastObjectReader<'a> {
    pub fn new(common_dir: &'a Path) -> Self {
        Self { common_dir }
    }

    fn has_alternates(&self) -> bool {
        self.common_dir
            .join("objects")
            .join("info")
            .join("alternates")
            .exists()
    }

    fn object_path(&self, oid: &str) -> Option<std::path::PathBuf> {
        if !is_valid_git_oid(oid) {
            return None;
        }
        Some(
            self.common_dir
                .join("objects")
                .join(&oid[..2])
                .join(&oid[2..]),
        )
    }

    fn object_decoder(&self, oid: &str) -> Option<ZlibDecoder<fs::File>> {
        if self.has_alternates() {
            return None;
        }
        let path = self.object_path(oid)?;
        let file = fs::File::open(path).ok()?;
        if file.metadata().ok()?.len() > MAX_LOOSE_OBJECT_COMPRESSED_BYTES {
            return None;
        }
        Some(ZlibDecoder::new(file))
    }

    fn decompress_object_with_limit(&self, oid: &str, max_bytes: u64) -> Option<Vec<u8>> {
        let decoder = self.object_decoder(oid)?;
        let mut decompressed = Vec::new();
        decoder
            .take(max_bytes + 1)
            .read_to_end(&mut decompressed)
            .ok()?;
        (decompressed.len() as u64 <= max_bytes).then_some(decompressed)
    }

    fn read_object_header(&self, oid: &str) -> Option<Vec<u8>> {
        let decoder = self.object_decoder(oid)?;
        let mut prefix = Vec::new();
        decoder
            .take(MAX_LOOSE_OBJECT_HEADER_BYTES)
            .read_to_end(&mut prefix)
            .ok()?;
        Some(prefix)
    }

    /// Read just the type from a loose object header without fully decompressing the content.
    pub fn try_read_object_type(&self, oid: &str) -> Option<String> {
        let data = self.read_object_header(oid)?;
        let null_pos = data.iter().position(|&b| b == 0)?;
        let header = std::str::from_utf8(&data[..null_pos]).ok()?;
        let type_str = header.split(' ').next()?;
        Some(type_str.to_string())
    }

    /// Read a loose blob object's content.
    ///
    /// Returns None if the object doesn't exist (packed), isn't a blob, or can't be read.
    pub fn try_read_blob(&self, oid: &str) -> Option<Vec<u8>> {
        let mut data =
            self.decompress_object_with_limit(oid, MAX_LOOSE_OBJECT_DECOMPRESSED_BYTES)?;
        let null_pos = data.iter().position(|&b| b == 0)?;
        let header = std::str::from_utf8(&data[..null_pos]).ok()?;
        if !header.starts_with("blob ") {
            return None;
        }
        let content_start = null_pos + 1;
        let content_len = data.len().saturating_sub(content_start);
        data.copy_within(content_start.., 0);
        data.truncate(content_len);
        Some(data)
    }

    /// Read a loose commit object and extract its tree OID.
    ///
    /// Commit format after header: `tree {hex-oid}\n...`
    pub fn try_read_commit_tree_oid(&self, commit_oid: &str) -> Option<String> {
        let data = self.read_object_header(commit_oid)?;
        let null_pos = data.iter().position(|&b| b == 0)?;
        let header = std::str::from_utf8(&data[..null_pos]).ok()?;
        if !header.starts_with("commit ") {
            return None;
        }
        let body = &data[null_pos + 1..];
        let first_line_end = body.iter().position(|byte| *byte == b'\n')?;
        let first_line = std::str::from_utf8(&body[..first_line_end]).ok()?;
        let tree_oid = first_line.strip_prefix("tree ")?;
        let tree_oid = tree_oid.trim();
        if is_valid_git_oid(tree_oid) {
            Some(tree_oid.to_string())
        } else {
            None
        }
    }

    /// Traverse a tree (and subtrees) to find the blob OID at the given path.
    ///
    /// For "src/main.rs", reads the root tree, finds "src" subtree, reads it,
    /// then finds "main.rs" blob entry.
    ///
    /// Returns None if any tree along the path is packed or the path doesn't exist.
    pub fn try_tree_entry_for_path(&self, tree_oid: &str, path: &Path) -> Option<String> {
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();

        if components.is_empty() {
            return None;
        }

        let mut current_tree_oid = tree_oid.to_string();

        for (i, component) in components.iter().enumerate() {
            let is_last = i == components.len() - 1;
            let entry_oid = self.find_tree_entry(&current_tree_oid, component)?;

            if is_last {
                return Some(entry_oid);
            }
            // Intermediate component must be a subtree
            current_tree_oid = entry_oid;
        }

        None
    }

    /// Find a named entry in a tree object, returning its OID.
    fn find_tree_entry(&self, tree_oid: &str, name: &str) -> Option<String> {
        let data =
            self.decompress_object_with_limit(tree_oid, MAX_LOOSE_OBJECT_DECOMPRESSED_BYTES)?;
        let null_pos = data.iter().position(|&b| b == 0)?;
        let header = std::str::from_utf8(&data[..null_pos]).ok()?;
        if !header.starts_with("tree ") {
            return None;
        }

        let hash_len = oid_byte_len(tree_oid);
        let entries_data = &data[null_pos + 1..];
        self.parse_tree_entries_for_name(entries_data, name, hash_len)
    }

    /// Parse binary tree entries to find an entry by name.
    ///
    /// Tree entry format: `{mode} {name}\0{raw-binary-hash}`
    fn parse_tree_entries_for_name(
        &self,
        mut data: &[u8],
        target_name: &str,
        hash_len: usize,
    ) -> Option<String> {
        while !data.is_empty() {
            // Find the space separating mode from name
            let space_pos = data.iter().position(|&b| b == b' ')?;
            // Find the null byte after name
            let null_pos = data[space_pos + 1..].iter().position(|&b| b == 0)?;
            let null_pos = space_pos + 1 + null_pos;

            let name_bytes = &data[space_pos + 1..null_pos];
            let name = std::str::from_utf8(name_bytes).ok()?;

            // The hash follows the null byte
            let hash_start = null_pos + 1;
            if data.len() < hash_start + hash_len {
                return None;
            }
            let hash_bytes = &data[hash_start..hash_start + hash_len];

            if name == target_name {
                let oid = hash_bytes
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<String>();
                return Some(oid);
            }

            data = &data[hash_start + hash_len..];
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_git_dir() -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("refs/heads")).unwrap();
        fs::create_dir_all(temp.path().join("objects")).unwrap();
        temp
    }

    fn write_loose_object(git_dir: &Path, sha: &str, obj_type: &str, content: &[u8]) {
        let obj_dir = git_dir.join("objects").join(&sha[..2]);
        fs::create_dir_all(&obj_dir).unwrap();

        let header = format!("{} {}\0", obj_type, content.len());
        let mut full_content = header.as_bytes().to_vec();
        full_content.extend_from_slice(content);

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&full_content).unwrap();
        let compressed = encoder.finish().unwrap();

        let obj_path = obj_dir.join(&sha[2..]);
        fs::write(obj_path, compressed).unwrap();
    }

    // ===== FastRefReader tests =====

    #[test]
    fn test_read_head_symbolic_ref() {
        let temp = setup_git_dir();
        fs::write(temp.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());
        let result = reader.try_read_head();
        assert_eq!(
            result,
            Some(HeadKind::Symbolic("refs/heads/main".to_string()))
        );
    }

    #[test]
    fn test_read_head_detached() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        fs::write(temp.path().join("HEAD"), format!("{}\n", sha)).unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());
        let result = reader.try_read_head();
        assert_eq!(result, Some(HeadKind::Detached(sha.to_string())));
    }

    #[test]
    fn test_read_head_invalid_format_returns_none() {
        let temp = setup_git_dir();
        fs::write(temp.path().join("HEAD"), "garbage content\n").unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());
        assert_eq!(reader.try_read_head(), None);
    }

    #[test]
    fn test_read_head_missing_returns_none() {
        let temp = TempDir::new().unwrap();
        let reader = FastRefReader::new(temp.path(), temp.path());
        assert_eq!(reader.try_read_head(), None);
    }

    #[test]
    fn test_oversized_head_returns_none() {
        let temp = setup_git_dir();
        let contents = format!("ref: refs/heads/main{}", " ".repeat(4 * 1_024));
        fs::write(temp.path().join("HEAD"), contents).unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());

        assert_eq!(reader.try_read_head(), None);
    }

    #[test]
    fn test_resolve_loose_ref() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        fs::write(temp.path().join("refs/heads/main"), format!("{}\n", sha)).unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());
        let result = reader.try_resolve_ref("refs/heads/main");
        assert_eq!(result, Some(sha.to_string()));
    }

    #[test]
    fn test_oversized_loose_ref_returns_none() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        fs::write(
            temp.path().join("refs/heads/main"),
            format!("{sha}{}", " ".repeat(4 * 1_024)),
        )
        .unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());

        assert_eq!(reader.try_resolve_ref("refs/heads/main"), None);
    }

    #[test]
    fn test_resolve_packed_ref() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        let packed_content = format!(
            "# pack-refs with: peeled fully-peeled sorted\n{} refs/heads/packed-branch\n",
            sha
        );
        fs::write(temp.path().join("packed-refs"), packed_content).unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());
        let result = reader.try_resolve_ref("refs/heads/packed-branch");
        assert_eq!(result, Some(sha.to_string()));
    }

    #[test]
    fn test_oversized_packed_refs_returns_none() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        let mut packed = format!("{sha} refs/heads/main\n").into_bytes();
        packed.resize(1_024 * 1_024 + 1, b'\n');
        fs::write(temp.path().join("packed-refs"), packed).unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());

        assert_eq!(reader.try_resolve_ref("refs/heads/main"), None);
    }

    #[test]
    fn test_resolve_ref_not_found() {
        let temp = setup_git_dir();
        let reader = FastRefReader::new(temp.path(), temp.path());
        assert_eq!(reader.try_resolve_ref("refs/heads/nonexistent"), None);
    }

    #[test]
    fn test_resolve_symbolic_ref_indirection() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        fs::create_dir_all(temp.path().join("refs/remotes/origin")).unwrap();
        fs::write(
            temp.path().join("refs/remotes/origin/HEAD"),
            "ref: refs/remotes/origin/main\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("refs/remotes/origin/main"),
            format!("{}\n", sha),
        )
        .unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());
        let result = reader.try_resolve_ref("refs/remotes/origin/HEAD");
        assert_eq!(result, Some(sha.to_string()));
    }

    #[test]
    fn test_resolve_head_resolves_through_symbolic() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        fs::write(temp.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(temp.path().join("refs/heads/main"), format!("{}\n", sha)).unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());
        let result = reader.try_resolve_ref("HEAD");
        assert_eq!(result, Some(sha.to_string()));
    }

    #[test]
    fn test_resolve_ref_worktree_common_dir_priority() {
        // Simulate a linked worktree: refs live in common_dir, not git_dir
        let common = setup_git_dir();
        let worktree_git_dir = TempDir::new().unwrap();
        fs::create_dir_all(worktree_git_dir.path()).unwrap();

        let sha = "abc123def456789012345678901234567890abcd";
        fs::write(common.path().join("refs/heads/main"), format!("{}\n", sha)).unwrap();

        let reader = FastRefReader::new(worktree_git_dir.path(), common.path());
        let result = reader.try_resolve_ref("refs/heads/main");
        assert_eq!(result, Some(sha.to_string()));
    }

    #[test]
    fn test_resolve_ref_loose_in_git_dir_over_packed() {
        let temp = setup_git_dir();
        let loose_sha = "1111111111111111111111111111111111111111";
        let packed_sha = "2222222222222222222222222222222222222222";

        fs::write(
            temp.path().join("refs/heads/main"),
            format!("{}\n", loose_sha),
        )
        .unwrap();
        let packed_content = format!("# pack-refs with: peeled\n{} refs/heads/main\n", packed_sha);
        fs::write(temp.path().join("packed-refs"), packed_content).unwrap();

        let reader = FastRefReader::new(temp.path(), temp.path());
        let result = reader.try_resolve_ref("refs/heads/main");
        assert_eq!(result, Some(loose_sha.to_string()));
    }

    // ===== FastObjectReader tests =====

    #[test]
    fn test_read_loose_blob() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        let content = b"Hello, World!";
        write_loose_object(temp.path(), sha, "blob", content);

        let reader = FastObjectReader::new(temp.path());
        let result = reader.try_read_blob(sha);
        assert_eq!(result, Some(content.to_vec()));
    }

    #[test]
    fn test_read_nonexistent_blob() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";

        let reader = FastObjectReader::new(temp.path());
        assert_eq!(reader.try_read_blob(sha), None);
    }

    #[test]
    fn test_read_commit_as_blob_returns_none() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        let content =
            b"tree def456789012345678901234567890abcdef01\nauthor Test <test@example.com>";
        write_loose_object(temp.path(), sha, "commit", content);

        let reader = FastObjectReader::new(temp.path());
        assert_eq!(reader.try_read_blob(sha), None);
    }

    #[test]
    fn test_read_object_type() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        write_loose_object(temp.path(), sha, "blob", b"content");

        let reader = FastObjectReader::new(temp.path());
        assert_eq!(reader.try_read_object_type(sha), Some("blob".to_string()));
    }

    #[test]
    fn test_object_type_reads_header_without_expanding_oversized_blob() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        write_loose_object(temp.path(), sha, "blob", &vec![b'x'; 4 * 1_024 * 1_024 + 1]);

        let reader = FastObjectReader::new(temp.path());

        assert_eq!(reader.try_read_object_type(sha), Some("blob".to_string()));
        assert!(reader.try_read_blob(sha).is_none());
    }

    #[test]
    fn test_read_commit_tree_oid() {
        let temp = setup_git_dir();
        let commit_sha = "abc123def456789012345678901234567890abcd";
        let tree_sha = "def456789012345678901234567890abcdef0123";
        let commit_body = format!(
            "tree {}\nparent 0000000000000000000000000000000000000000\nauthor A <a@b.c> 1 +0000\ncommitter A <a@b.c> 1 +0000\n\nmessage\n",
            tree_sha
        );
        write_loose_object(temp.path(), commit_sha, "commit", commit_body.as_bytes());

        let reader = FastObjectReader::new(temp.path());
        let result = reader.try_read_commit_tree_oid(commit_sha);
        assert_eq!(result, Some(tree_sha.to_string()));
    }

    #[test]
    fn test_commit_tree_oid_ignores_truncated_utf8_after_tree_line() {
        let temp = setup_git_dir();
        let commit_sha = "abc123def456789012345678901234567890abcd";
        let tree_sha = "def456789012345678901234567890abcdef0123";
        let tree_line = format!("tree {tree_sha}\n");
        let content_len = 5_000;
        let object_header_len = format!("commit {content_len}\0").len();
        let split_offset = MAX_LOOSE_OBJECT_HEADER_BYTES as usize - object_header_len - 1;
        let mut commit_body = tree_line.into_bytes();
        commit_body.resize(split_offset, b'x');
        commit_body.extend_from_slice(&[0xc3, 0xa9]);
        commit_body.resize(content_len, b'y');
        assert_eq!(
            object_header_len + split_offset + 1,
            MAX_LOOSE_OBJECT_HEADER_BYTES as usize
        );
        write_loose_object(temp.path(), commit_sha, "commit", &commit_body);

        let reader = FastObjectReader::new(temp.path());

        assert_eq!(
            reader.try_read_commit_tree_oid(commit_sha),
            Some(tree_sha.to_string())
        );
    }

    #[test]
    fn test_tree_entry_for_path_single_level() {
        let temp = setup_git_dir();
        let tree_sha = "abc123def456789012345678901234567890abcd";
        let blob_sha_bytes: [u8; 20] = [
            0xde, 0xf4, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x78, 0x90, 0x12, 0x34, 0x56, 0x78,
            0x90, 0xab, 0xcd, 0xef, 0x01, 0x23,
        ];
        let expected_blob_oid = "def456789012345678901234567890abcdef0123";

        // Build tree content: "100644 file.txt\0<20-byte-sha>"
        let mut tree_content = Vec::new();
        tree_content.extend_from_slice(b"100644 file.txt\0");
        tree_content.extend_from_slice(&blob_sha_bytes);

        write_loose_object(temp.path(), tree_sha, "tree", &tree_content);

        let reader = FastObjectReader::new(temp.path());
        let result = reader.try_tree_entry_for_path(tree_sha, Path::new("file.txt"));
        assert_eq!(result, Some(expected_blob_oid.to_string()));
    }

    #[test]
    fn test_tree_entry_for_path_nested() {
        let temp = setup_git_dir();

        // Create blob
        let blob_sha_bytes: [u8; 20] = [
            0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
            0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
        ];
        let expected_blob_oid = "aabbccddeeff00112233445566778899aabbccdd";

        // Create subtree containing the blob
        let subtree_sha = "1111111111111111111111111111111111111111";
        let subtree_sha_bytes: [u8; 20] = [
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        ];

        let mut subtree_content = Vec::new();
        subtree_content.extend_from_slice(b"100644 main.rs\0");
        subtree_content.extend_from_slice(&blob_sha_bytes);
        write_loose_object(temp.path(), subtree_sha, "tree", &subtree_content);

        // Create root tree containing the subtree
        let root_tree_sha = "2222222222222222222222222222222222222222";
        let mut root_content = Vec::new();
        root_content.extend_from_slice(b"40000 src\0");
        root_content.extend_from_slice(&subtree_sha_bytes);
        write_loose_object(temp.path(), root_tree_sha, "tree", &root_content);

        let reader = FastObjectReader::new(temp.path());
        let result = reader.try_tree_entry_for_path(root_tree_sha, Path::new("src/main.rs"));
        assert_eq!(result, Some(expected_blob_oid.to_string()));
    }

    #[test]
    fn test_tree_entry_for_path_not_found() {
        let temp = setup_git_dir();
        let tree_sha = "abc123def456789012345678901234567890abcd";
        let blob_sha_bytes: [u8; 20] = [0xde; 20];

        let mut tree_content = Vec::new();
        tree_content.extend_from_slice(b"100644 other.txt\0");
        tree_content.extend_from_slice(&blob_sha_bytes);
        write_loose_object(temp.path(), tree_sha, "tree", &tree_content);

        let reader = FastObjectReader::new(temp.path());
        let result = reader.try_tree_entry_for_path(tree_sha, Path::new("missing.txt"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_alternates_causes_fallback() {
        let temp = setup_git_dir();
        let sha = "abc123def456789012345678901234567890abcd";
        write_loose_object(temp.path(), sha, "blob", b"content");

        // Create alternates file
        fs::create_dir_all(temp.path().join("objects/info")).unwrap();
        fs::write(
            temp.path().join("objects/info/alternates"),
            "/some/other/objects\n",
        )
        .unwrap();

        let reader = FastObjectReader::new(temp.path());
        assert_eq!(reader.try_read_blob(sha), None);
    }

    #[test]
    fn test_invalid_oid_returns_none() {
        let temp = setup_git_dir();
        let reader = FastObjectReader::new(temp.path());
        assert_eq!(reader.try_read_blob("not-a-valid-oid"), None);
        assert_eq!(reader.try_read_blob(""), None);
        assert_eq!(reader.try_read_blob("abc"), None);
    }
}
