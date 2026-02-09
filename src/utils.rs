use crate::error::GitAiError;
use crate::git::diff_tree_to_tree::Diff;
use std::io::IsTerminal;
use std::path::PathBuf;

/// Check if debug logging is enabled via environment variable
///
/// This is checked once at module initialization to avoid repeated environment variable lookups.
static DEBUG_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static DEBUG_PERFORMANCE_LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
static IS_TERMINAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn is_debug_enabled() -> bool {
    *DEBUG_ENABLED.get_or_init(|| {
        (cfg!(debug_assertions)
            || std::env::var("GIT_AI_DEBUG").unwrap_or_default() == "1"
            || std::env::var("GIT_AI_DEBUG_PERFORMANCE").unwrap_or_default() != "")
            && std::env::var("GIT_AI_DEBUG").unwrap_or_default() != "0"
    })
}

fn is_debug_performance_enabled() -> bool {
    debug_performance_level() >= 1
}

fn debug_performance_level() -> u8 {
    *DEBUG_PERFORMANCE_LEVEL.get_or_init(|| {
        std::env::var("GIT_AI_DEBUG_PERFORMANCE")
            .unwrap_or_default()
            .parse::<u8>()
            .unwrap_or(0)
    })
}

pub fn debug_performance_log(msg: &str) {
    if is_debug_performance_enabled() {
        eprintln!("\x1b[1;33m[git-ai (perf)]\x1b[0m {}", msg);
    }
}

pub fn debug_performance_log_structured(json: serde_json::Value) {
    if debug_performance_level() >= 2 {
        eprintln!("\x1b[1;33m[git-ai (perf-json)]\x1b[0m {}", json);
    }
}

/// Debug logging utility function
///
/// Prints debug messages with a colored prefix when debug assertions are enabled or when
/// the `GIT_AI_DEBUG` environment variable is set to "1".
///
/// # Arguments
///
/// * `msg` - The debug message to print
pub fn debug_log(msg: &str) {
    if is_debug_enabled() {
        eprintln!("\x1b[1;33m[git-ai]\x1b[0m {}", msg);
    }
}

/// Print a git diff in a readable format
///
/// Prints the diff between two commits/trees showing which files changed and their status.
/// This is useful for debugging and understanding what changes occurred.
///
/// # Arguments
///
/// * `diff` - The git diff object to print
/// * `old_label` - Label for the "old" side (e.g., commit SHA or description)
/// * `new_label` - Label for the "new" side (e.g., commit SHA or description)
pub fn _print_diff(diff: &Diff, old_label: &str, new_label: &str) {
    println!("Diff between {} and {}:", old_label, new_label);

    let mut file_count = 0;
    for delta in diff.deltas() {
        file_count += 1;
        let old_file = delta.old_file().path().unwrap_or(std::path::Path::new(""));
        let new_file = delta.new_file().path().unwrap_or(std::path::Path::new(""));
        let status = delta.status();

        println!(
            "  File {}: {} -> {} (status: {:?})",
            file_count,
            old_file.display(),
            new_file.display(),
            status
        );
    }

    if file_count == 0 {
        println!("  No changes between {} and {}", old_label, new_label);
    }
}

#[inline]
pub fn normalize_to_posix(path: &str) -> String {
    path.replace('\\', "/")
}

pub fn current_git_ai_exe() -> Result<PathBuf, GitAiError> {
    let path = std::env::current_exe()?;

    // Get platform-specific executable names
    let git_name = if cfg!(windows) { "git.exe" } else { "git" };
    let git_ai_name = if cfg!(windows) {
        "git-ai.exe"
    } else {
        "git-ai"
    };

    // Check if the filename matches the git executable name for this platform
    if let Some(file_name) = path.file_name().and_then(|n| n.to_str())
        && file_name == git_name
    {
        // Try replacing with git-ai executable name for this platform
        let git_ai_path = path.with_file_name(git_ai_name);

        // Check if the git-ai file exists
        if git_ai_path.exists() {
            return Ok(git_ai_path);
        }

        // If it doesn't exist, return the git-ai executable name as a PathBuf
        return Ok(PathBuf::from(git_ai_name));
    }

    Ok(path)
}

pub fn is_interactive_terminal() -> bool {
    *IS_TERMINAL.get_or_init(|| std::io::stdin().is_terminal())
}

/// A cross-platform exclusive file lock.
///
/// Holds an exclusive advisory lock (Unix) or exclusive-access file handle (Windows)
/// for the lifetime of the struct. The lock is automatically released when dropped
/// or when the process exits.
pub struct LockFile {
    _file: std::fs::File,
}

impl LockFile {
    /// Try to acquire an exclusive lock on the given path.
    /// Returns `Some(LockFile)` if successful, `None` if another process holds the lock.
    pub fn try_acquire(path: &std::path::Path) -> Option<Self> {
        let file = try_lock_exclusive(path)?;
        Some(Self { _file: file })
    }
}

#[cfg(unix)]
#[allow(clippy::suspicious_open_options)]
fn try_lock_exclusive(path: &std::path::Path) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .ok()?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return None;
    }
    Some(file)
}

#[cfg(windows)]
#[allow(clippy::suspicious_open_options)]
fn try_lock_exclusive(path: &std::path::Path) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .share_mode(0)
        .open(path)
        .ok()
}

/// Windows-specific flag to prevent console window creation
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;
/// Unescape a git-quoted path that may contain octal escape sequences.
///
/// Git quotes filenames containing non-ASCII characters (and some special characters)
/// using C-style escaping with octal sequences. For example, a Chinese filename like
/// "中文.txt" would appear as `"\344\270\255\346\226\207.txt"` in git output.
///
/// This function handles:
/// - Quoted paths: removes surrounding quotes and unescapes content
/// - Octal escapes: converts `\NNN` sequences back to UTF-8 bytes
/// - Other escapes: `\\`, `\"`, `\n`, `\t`, etc.
/// - Unquoted paths: returned as-is
///
/// # Examples
///
/// ```
/// use git_ai::utils::unescape_git_path;
///
/// // Unquoted path - returned as-is
/// assert_eq!(unescape_git_path("simple.txt"), "simple.txt");
///
/// // Quoted path with spaces
/// assert_eq!(unescape_git_path("\"path with spaces.txt\""), "path with spaces.txt");
///
/// // Chinese characters encoded as octal
/// assert_eq!(unescape_git_path("\"\\344\\270\\255\\346\\226\\207.txt\""), "中文.txt");
/// ```
pub fn unescape_git_path(path: &str) -> String {
    // If not quoted, return as-is
    if !path.starts_with('"') || !path.ends_with('"') {
        return path.to_string();
    }

    // Remove surrounding quotes
    let inner = &path[1..path.len() - 1];

    // Parse escape sequences and collect bytes
    let mut bytes: Vec<u8> = Vec::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('\\') => {
                    chars.next();
                    bytes.push(b'\\');
                }
                Some('"') => {
                    chars.next();
                    bytes.push(b'"');
                }
                Some('n') => {
                    chars.next();
                    bytes.push(b'\n');
                }
                Some('t') => {
                    chars.next();
                    bytes.push(b'\t');
                }
                Some('r') => {
                    chars.next();
                    bytes.push(b'\r');
                }
                Some(d) if d.is_ascii_digit() => {
                    // Octal escape sequence: \NNN (1-3 octal digits)
                    let mut octal = String::new();
                    for _ in 0..3 {
                        if let Some(&d) = chars.peek() {
                            if d.is_ascii_digit() && d <= '7' {
                                octal.push(chars.next().unwrap());
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    if let Ok(byte_val) = u8::from_str_radix(&octal, 8) {
                        bytes.push(byte_val);
                    }
                }
                _ => {
                    // Unknown escape - keep the backslash
                    bytes.push(b'\\');
                }
            }
        } else {
            // Regular character - encode as UTF-8
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            bytes.extend_from_slice(encoded.as_bytes());
        }
    }

    // Convert bytes to UTF-8 string
    String::from_utf8(bytes).unwrap_or_else(|e| {
        // If invalid UTF-8, try lossy conversion
        String::from_utf8_lossy(e.as_bytes()).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // LockFile Tests
    // =========================================================================

    #[test]
    fn test_lockfile_acquire_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");
        let lock = LockFile::try_acquire(&lock_path);
        assert!(lock.is_some(), "should acquire lock on a fresh path");
    }

    #[test]
    fn test_lockfile_second_acquire_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");
        let _first = LockFile::try_acquire(&lock_path).expect("first acquire should succeed");
        let second = LockFile::try_acquire(&lock_path);
        assert!(second.is_none(), "second acquire should be blocked");
    }

    #[test]
    fn test_lockfile_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");
        {
            let _lock = LockFile::try_acquire(&lock_path).expect("first acquire should succeed");
            // _lock is dropped here
        }
        let second = LockFile::try_acquire(&lock_path);
        assert!(
            second.is_some(),
            "should acquire lock after previous holder is dropped"
        );
    }

    #[test]
    fn test_lockfile_nonexistent_parent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("no_such_dir").join("test.lock");
        let lock = LockFile::try_acquire(&lock_path);
        assert!(
            lock.is_none(),
            "should return None when parent directory does not exist"
        );
    }

    // =========================================================================
    // unescape_git_path Tests
    // =========================================================================

    #[test]
    fn test_unescape_git_path_simple() {
        // Unquoted path - no change
        assert_eq!(unescape_git_path("simple.txt"), "simple.txt");
        assert_eq!(unescape_git_path("path/to/file.rs"), "path/to/file.rs");
    }

    #[test]
    fn test_unescape_git_path_quoted_with_spaces() {
        // Quoted path with spaces
        assert_eq!(
            unescape_git_path("\"path with spaces.txt\""),
            "path with spaces.txt"
        );
        assert_eq!(
            unescape_git_path("\"dir name/file name.txt\""),
            "dir name/file name.txt"
        );
    }

    #[test]
    fn test_unescape_git_path_chinese_characters() {
        // Chinese characters "中文" encoded as octal: \344\270\255\346\226\207
        assert_eq!(
            unescape_git_path("\"\\344\\270\\255\\346\\226\\207.txt\""),
            "中文.txt"
        );

        // More complex Chinese filename: "中文文件.txt"
        // 中 = \344\270\255, 文 = \346\226\207, 件 = \344\273\266
        assert_eq!(
            unescape_git_path(
                "\"\\344\\270\\255\\346\\226\\207\\346\\226\\207\\344\\273\\266.txt\""
            ),
            "中文文件.txt"
        );
    }

    #[test]
    fn test_unescape_git_path_emoji() {
        // Emoji "🚀" (rocket) = U+1F680 = \360\237\232\200 in octal UTF-8
        assert_eq!(unescape_git_path("\"\\360\\237\\232\\200.txt\""), "🚀.txt");

        // Emoji "😀" (grinning face) = U+1F600 = \360\237\230\200 in octal UTF-8
        assert_eq!(unescape_git_path("\"\\360\\237\\230\\200.txt\""), "😀.txt");

        // Mixed: "test_🎉_file.txt" where 🎉 = \360\237\216\211
        assert_eq!(
            unescape_git_path("\"test_\\360\\237\\216\\211_file.txt\""),
            "test_🎉_file.txt"
        );
    }

    #[test]
    fn test_unescape_git_path_escaped_characters() {
        // Escaped backslash
        assert_eq!(
            unescape_git_path("\"path\\\\with\\\\slashes\""),
            "path\\with\\slashes"
        );

        // Escaped quotes
        assert_eq!(unescape_git_path("\"file\\\"name.txt\""), "file\"name.txt");

        // Escaped newline and tab
        assert_eq!(unescape_git_path("\"line1\\nline2\""), "line1\nline2");
        assert_eq!(unescape_git_path("\"col1\\tcol2\""), "col1\tcol2");
    }

    #[test]
    fn test_unescape_git_path_mixed_content() {
        // Mix of ASCII, Chinese, and escapes
        assert_eq!(
            unescape_git_path("\"src/\\344\\270\\255\\346\\226\\207/file.txt\""),
            "src/中文/file.txt"
        );
    }

    // =========================================================================
    // Phase 1: CJK Extended Coverage Tests
    // =========================================================================

    #[test]
    fn test_unescape_japanese_hiragana() {
        // Japanese Hiragana "ひらがな" = \343\201\262\343\202\211\343\201\214\343\201\252
        assert_eq!(
            unescape_git_path(
                "\"\\343\\201\\262\\343\\202\\211\\343\\201\\214\\343\\201\\252.txt\""
            ),
            "ひらがな.txt"
        );
    }

    #[test]
    fn test_unescape_japanese_katakana() {
        // Japanese Katakana "カタカナ" = \343\202\253\343\202\277\343\202\253\343\203\212
        assert_eq!(
            unescape_git_path(
                "\"\\343\\202\\253\\343\\202\\277\\343\\202\\253\\343\\203\\212.txt\""
            ),
            "カタカナ.txt"
        );
    }

    #[test]
    fn test_unescape_korean_hangul() {
        // Korean Hangul "한글" = \355\225\234\352\270\200
        assert_eq!(
            unescape_git_path("\"\\355\\225\\234\\352\\270\\200.txt\""),
            "한글.txt"
        );
    }

    #[test]
    fn test_unescape_traditional_chinese() {
        // Traditional Chinese "繁體" = \347\271\201\351\253\224
        assert_eq!(
            unescape_git_path("\"\\347\\271\\201\\351\\253\\224.txt\""),
            "繁體.txt"
        );
    }

    #[test]
    fn test_unescape_mixed_cjk() {
        // Mixed CJK: "日中韓" (Japanese, Chinese, Korean characters mixed)
        // 日 = \346\227\245, 中 = \344\270\255, 韓 = \351\237\223
        assert_eq!(
            unescape_git_path("\"\\346\\227\\245\\344\\270\\255\\351\\237\\223.txt\""),
            "日中韓.txt"
        );
    }

    // =========================================================================
    // Phase 2: RTL Scripts Tests (Arabic, Hebrew, Persian, Urdu)
    // =========================================================================

    #[test]
    fn test_unescape_arabic() {
        // Arabic "مرحبا" (marhaba = hello)
        // م = \331\205, ر = \330\261, ح = \330\255, ب = \330\250, ا = \330\247
        assert_eq!(
            unescape_git_path("\"\\331\\205\\330\\261\\330\\255\\330\\250\\330\\247.txt\""),
            "مرحبا.txt"
        );
    }

    #[test]
    fn test_unescape_hebrew() {
        // Hebrew "שלום" (shalom = hello/peace)
        // ש = \327\251, ל = \327\234, ו = \327\225, ם = \327\235
        assert_eq!(
            unescape_git_path("\"\\327\\251\\327\\234\\327\\225\\327\\235.txt\""),
            "שלום.txt"
        );
    }

    #[test]
    fn test_unescape_persian() {
        // Persian "فارسی" (farsi)
        // ف = \331\201, ا = \330\247, ر = \330\261, س = \330\263, ی = \333\214
        assert_eq!(
            unescape_git_path("\"\\331\\201\\330\\247\\330\\261\\330\\263\\333\\214.txt\""),
            "فارسی.txt"
        );
    }

    #[test]
    fn test_unescape_urdu() {
        // Urdu "اردو" (urdu)
        // ا = \330\247, ر = \330\261, د = \330\257, و = \331\210
        assert_eq!(
            unescape_git_path("\"\\330\\247\\330\\261\\330\\257\\331\\210.txt\""),
            "اردو.txt"
        );
    }

    #[test]
    fn test_unescape_mixed_rtl_ltr() {
        // Mixed RTL/LTR: "test_مرحبا_file" (ASCII + Arabic + ASCII)
        assert_eq!(
            unescape_git_path(
                "\"test_\\331\\205\\330\\261\\330\\255\\330\\250\\330\\247_file.txt\""
            ),
            "test_مرحبا_file.txt"
        );
    }

    // =========================================================================
    // Phase 3: Indic Scripts Tests (Hindi, Tamil, Bengali, Telugu, Gujarati)
    // =========================================================================

    #[test]
    fn test_unescape_hindi_devanagari() {
        // Hindi "हिंदी" (Hindi in Devanagari script)
        // ह = \340\244\271, ि = \340\244\277, ं = \340\244\202, द = \340\244\246, ी = \340\245\200
        assert_eq!(
            unescape_git_path(
                "\"\\340\\244\\271\\340\\244\\277\\340\\244\\202\\340\\244\\246\\340\\245\\200.txt\""
            ),
            "हिंदी.txt"
        );
    }

    #[test]
    fn test_unescape_tamil() {
        // Tamil "தமிழ்" (Tamil)
        // த = \340\256\244, ம = \340\256\256, ி = \340\256\277, ழ = \340\256\264, ் = \340\257\215
        assert_eq!(
            unescape_git_path(
                "\"\\340\\256\\244\\340\\256\\256\\340\\256\\277\\340\\256\\264\\340\\257\\215.txt\""
            ),
            "தமிழ்.txt"
        );
    }

    #[test]
    fn test_unescape_bengali() {
        // Bengali "বাংলা" (Bangla)
        // ব = \340\246\254, া = \340\246\276, ং = \340\246\202, ল = \340\246\262, া = \340\246\276
        assert_eq!(
            unescape_git_path(
                "\"\\340\\246\\254\\340\\246\\276\\340\\246\\202\\340\\246\\262\\340\\246\\276.txt\""
            ),
            "বাংলা.txt"
        );
    }

    #[test]
    fn test_unescape_telugu() {
        // Telugu "తెలుగు" (Telugu)
        // త = \340\260\244, ె = \340\261\206, ల = \340\260\262, ు = \340\261\201, గ = \340\260\227, ు = \340\261\201
        assert_eq!(
            unescape_git_path(
                "\"\\340\\260\\244\\340\\261\\206\\340\\260\\262\\340\\261\\201\\340\\260\\227\\340\\261\\201.txt\""
            ),
            "తెలుగు.txt"
        );
    }

    #[test]
    fn test_unescape_gujarati() {
        // Gujarati "ગુજરાતી" (Gujarati)
        // ગ = \340\252\227, ુ = \340\253\201, જ = \340\252\234, ર = \340\252\260, ા = \340\252\276, ત = \340\252\244, ી = \340\253\200
        assert_eq!(
            unescape_git_path(
                "\"\\340\\252\\227\\340\\253\\201\\340\\252\\234\\340\\252\\260\\340\\252\\276\\340\\252\\244\\340\\253\\200.txt\""
            ),
            "ગુજરાતી.txt"
        );
    }

    // =========================================================================
    // Phase 4: Southeast Asian Scripts Tests (Thai, Vietnamese, Khmer, Lao)
    // =========================================================================

    #[test]
    fn test_unescape_thai() {
        // Thai "ไทย" (Thai)
        // ไ = \340\271\204, ท = \340\270\227, ย = \340\270\242
        assert_eq!(
            unescape_git_path("\"\\340\\271\\204\\340\\270\\227\\340\\270\\242.txt\""),
            "ไทย.txt"
        );
    }

    #[test]
    fn test_unescape_vietnamese() {
        // Vietnamese "tiếng" with tone marks
        // t = 't', i = 'i', ế = \341\272\277, n = 'n', g = 'g'
        assert_eq!(
            unescape_git_path("\"ti\\341\\272\\277ng.txt\""),
            "tiếng.txt"
        );
    }

    #[test]
    fn test_unescape_khmer() {
        // Khmer "ខ្មែរ" (Khmer)
        // ខ = \341\236\201, ្ = \341\237\222, ម = \341\236\230, ែ = \341\237\202, រ = \341\236\232
        assert_eq!(
            unescape_git_path(
                "\"\\341\\236\\201\\341\\237\\222\\341\\236\\230\\341\\237\\202\\341\\236\\232.txt\""
            ),
            "ខ្មែរ.txt"
        );
    }

    #[test]
    fn test_unescape_lao() {
        // Lao "ລາວ" (Lao)
        // ລ = \340\272\245, າ = \340\272\262, ວ = \340\272\247
        assert_eq!(
            unescape_git_path("\"\\340\\272\\245\\340\\272\\262\\340\\272\\247.txt\""),
            "ລາວ.txt"
        );
    }

    // =========================================================================
    // Phase 5: Cyrillic and Greek Scripts Tests
    // =========================================================================

    #[test]
    fn test_unescape_russian_cyrillic() {
        // Russian "Русский" (Russian)
        // Р = \320\240, у = \321\203, с = \321\201, к = \320\272, и = \320\270, й = \320\271
        assert_eq!(
            unescape_git_path(
                "\"\\320\\240\\321\\203\\321\\201\\321\\201\\320\\272\\320\\270\\320\\271.txt\""
            ),
            "Русский.txt"
        );
    }

    #[test]
    fn test_unescape_ukrainian_cyrillic() {
        // Ukrainian "Україна" (Ukraine)
        // У = \320\243, к = \320\272, р = \321\200, а = \320\260, ї = \321\227, н = \320\275, а = \320\260
        assert_eq!(
            unescape_git_path(
                "\"\\320\\243\\320\\272\\321\\200\\320\\260\\321\\227\\320\\275\\320\\260.txt\""
            ),
            "Україна.txt"
        );
    }

    #[test]
    fn test_unescape_greek() {
        // Greek "Ελλάδα" (Greece)
        // Ε = \316\225, λ = \316\273, λ = \316\273, ά = \316\254, δ = \316\264, α = \316\261
        assert_eq!(
            unescape_git_path(
                "\"\\316\\225\\316\\273\\316\\273\\316\\254\\316\\264\\316\\261.txt\""
            ),
            "Ελλάδα.txt"
        );
    }

    #[test]
    fn test_unescape_greek_polytonic() {
        // Greek polytonic "Ἑλληνική" (Hellenic with diacritics)
        // Ἑ = \341\274\231, λ = \316\273, λ = \316\273, η = \316\267, ν = \316\275, ι = \316\271, κ = \316\272, ή = \316\256
        assert_eq!(
            unescape_git_path(
                "\"\\341\\274\\231\\316\\273\\316\\273\\316\\267\\316\\275\\316\\271\\316\\272\\316\\256.txt\""
            ),
            "Ἑλληνική.txt"
        );
    }

    // =========================================================================
    // Phase 6: Extended Emoji Tests (ZWJ, skin tones, flags)
    // =========================================================================

    #[test]
    fn test_unescape_emoji_skin_tone() {
        // Emoji with skin tone modifier 👋🏽 = 👋 (U+1F44B) + 🏽 (U+1F3FD)
        // 👋 = \360\237\221\213, 🏽 = \360\237\217\275
        assert_eq!(
            unescape_git_path("\"\\360\\237\\221\\213\\360\\237\\217\\275.txt\""),
            "👋🏽.txt"
        );
    }

    #[test]
    fn test_unescape_emoji_zwj_sequence() {
        // ZWJ emoji sequence: 👨‍💻 (man technologist) = man + ZWJ + laptop
        // 👨 = \360\237\221\250, ZWJ = \342\200\215, 💻 = \360\237\222\273
        assert_eq!(
            unescape_git_path("\"\\360\\237\\221\\250\\342\\200\\215\\360\\237\\222\\273.txt\""),
            "👨‍💻.txt"
        );
    }

    #[test]
    fn test_unescape_emoji_flag() {
        // Flag emoji 🇯🇵 (Japan) = regional indicator J + regional indicator P
        // 🇯 = \360\237\207\257, 🇵 = \360\237\207\265
        assert_eq!(
            unescape_git_path("\"\\360\\237\\207\\257\\360\\237\\207\\265.txt\""),
            "🇯🇵.txt"
        );
    }

    #[test]
    fn test_unescape_multiple_emoji() {
        // Multiple emoji: 🚀🎉 (rocket + party)
        // 🚀 = \360\237\232\200, 🎉 = \360\237\216\211
        assert_eq!(
            unescape_git_path("\"\\360\\237\\232\\200\\360\\237\\216\\211.txt\""),
            "🚀🎉.txt"
        );
    }

    // =========================================================================
    // Phase 7: Special Unicode Characters Tests (math, currency, symbols)
    // =========================================================================

    #[test]
    fn test_unescape_math_symbols() {
        // Math symbols: ∑ (summation) = \342\210\221
        assert_eq!(unescape_git_path("\"\\342\\210\\221.txt\""), "∑.txt");
    }

    #[test]
    fn test_unescape_currency_symbols() {
        // Currency: € (euro) = \342\202\254
        assert_eq!(unescape_git_path("\"\\342\\202\\254.txt\""), "€.txt");
    }

    #[test]
    fn test_unescape_box_drawing() {
        // Box drawing: ┌ (box drawings light down and right) = \342\224\214
        assert_eq!(unescape_git_path("\"\\342\\224\\214.txt\""), "┌.txt");
    }

    #[test]
    fn test_unescape_dingbats() {
        // Dingbats: ✓ (check mark) = \342\234\223
        assert_eq!(unescape_git_path("\"\\342\\234\\223.txt\""), "✓.txt");
    }

    // =========================================================================
    // Phase 8: Unicode Normalization Tests (NFC vs NFD)
    // =========================================================================

    #[test]
    fn test_unescape_nfc_precomposed() {
        // NFC precomposed: é (U+00E9) = \303\251
        assert_eq!(unescape_git_path("\"caf\\303\\251.txt\""), "café.txt");
    }

    #[test]
    fn test_unescape_nfd_decomposed() {
        // NFD decomposed: e + combining acute (U+0065 + U+0301) = e + \314\201
        assert_eq!(
            unescape_git_path("\"cafe\\314\\201.txt\""),
            "cafe\u{0301}.txt"
        );
    }

    #[test]
    fn test_unescape_combining_diaeresis() {
        // Combining diaeresis: i + ̈ (U+0069 + U+0308) = i + \314\210
        assert_eq!(
            unescape_git_path("\"nai\\314\\210ve.txt\""),
            "nai\u{0308}ve.txt"
        );
    }

    #[test]
    fn test_unescape_angstrom() {
        // Å (A with ring above, U+00C5) = \303\205
        assert_eq!(
            unescape_git_path("\"\\303\\205ngstr\\303\\266m.txt\""),
            "Ångström.txt"
        );
    }
}
