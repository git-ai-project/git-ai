use std::fs;
use std::path::{Path, PathBuf};

const ALLOWED_RAW_OPEN_FILE: &str = "src/sqlite.rs";
const POLICY_TEST_FILE: &str = "tests/integration/sqlite_connection_policy.rs";

#[test]
fn sqlite_connections_go_through_memory_limited_helpers() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = ["src", "tests", "benches"];
    let forbidden_patterns = [
        "Connection::open(",
        "Connection::open_in_memory(",
        "Connection::open_with_flags(",
        "rusqlite::Connection::open(",
        "rusqlite::Connection::open_in_memory(",
        "rusqlite::Connection::open_with_flags(",
    ];

    let mut violations = Vec::new();
    for root in roots {
        collect_rust_files(&manifest_dir.join(root), &mut |path| {
            let relative = path.strip_prefix(&manifest_dir).unwrap();
            if relative == Path::new(ALLOWED_RAW_OPEN_FILE)
                || relative == Path::new(POLICY_TEST_FILE)
            {
                return;
            }

            let contents = fs::read_to_string(path).unwrap();
            for (line_number, line) in contents.lines().enumerate() {
                if forbidden_patterns
                    .iter()
                    .any(|pattern| line.contains(pattern))
                {
                    violations.push(format!("{}:{}", relative.display(), line_number + 1));
                }
            }
        });
    }

    assert!(
        violations.is_empty(),
        "SQLite connections must use src/sqlite.rs helpers so every open applies memory limits:\n{}",
        violations.join("\n")
    );
}

fn collect_rust_files(dir: &Path, visitor: &mut impl FnMut(&Path)) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, visitor);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            visitor(&path);
        }
    }
}
