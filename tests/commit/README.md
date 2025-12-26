# Commit Message Stats Tests

This directory contains tests for the commit message stats feature.

## Running Tests

### Shell Integration Tests

```bash
./tests/commit/run-message-stats-tests.sh
```

With cleanup disabled (for debugging):
```bash
./tests/commit/run-message-stats-tests.sh --no-cleanup
```

### Rust Integration Tests

```bash
cargo test --test commit_message_stats
```

### Unit Tests

```bash
cargo test commit_message
cargo test post_commit
```

## Test Coverage

| Test | Description | Expected Result |
|------|-------------|-----------------|
| 1 | Commit without AI code | No stats added |
| 2 | Pure AI code | Shows 100% AI |
| 3 | Mixed code | Shows 50% human, 50% AI |
| 4 | Delete-only commit | Shows "(no additions)" |
| 5 | Markdown format | Shows 🧠 and 🤖 |
| 6 | No AI + Markdown | No stats added |
| 7 | Disabled feature | No stats added |
| 8 | Custom template | Template applied |
| 9 | Git Notes sync | SHA changes, Notes updated |
| 10 | Full history | View all commits |

## Manual Testing

```bash
# 1. Create test repo
mkdir /tmp/test-stats && cd /tmp/test-stats
git init
git config user.name "Test"
git config user.email "test@test.com"
git config ai.commit-message-stats.enabled true

# 2. Test without AI
echo "test" > file.txt
git add file.txt
GIT_AI=/path/to/git-ai/target/debug/git-ai git commit -m "No AI"
# Verify: git log -1 --format='%B' should have no stats

# 3. Test with AI
echo "// AI" > ai.rs
git add ai.rs
/path/to/git-ai/target/debug/git-ai checkpoint mock_ai ai.rs
GIT_AI=/path/to/git-ai/target/debug/git-ai git commit -m "With AI"
# Verify: git log -1 --format='%B' should show stats
```

## Related Files

- `tests/commit_message_stats.rs` - Rust integration tests
- `src/authorship/commit_message.rs` - Formatting logic
- `src/authorship/post_commit.rs` - Post-commit integration
- `tests/commit/run-message-stats-tests.sh` - Shell integration tests

## Debugging

If tests fail:

1. Check detailed output (script shows this by default)
2. Use `--no-cleanup` to preserve test directory
3. Run git commands manually in test directory
4. Check `.git/ai/working_logs/` for checkpoint data
5. Check `refs/notes/ai` for Git Notes
