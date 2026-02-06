#!/usr/bin/env bats

# BATS test file for git-ai end-to-end scenarios
# https://github.com/bats-core/bats-core

setup() {
    # Create a temporary directory for each test
    export TEST_TEMP_DIR="$(mktemp -d)"
    export ORIGINAL_DIR="$(pwd)"
    cd "$TEST_TEMP_DIR"
    
    # Set up git-ai binary path - use debug build by default, fallback to release
    if [ -f "$ORIGINAL_DIR/target/debug/git-ai" ]; then
        export GIT_AI_BINARY="$ORIGINAL_DIR/target/debug/git-ai"
    elif [ -f "$ORIGINAL_DIR/target/release/git-ai" ]; then
        export GIT_AI_BINARY="$ORIGINAL_DIR/target/release/git-ai"
    else
        echo "ERROR: git-ai binary not found in target/debug or target/release" >&3
        echo "Please run 'cargo build' or 'cargo build --release' first" >&3
        exit 1
    fi
    
    # Create shell functions to alias git-ai and git commands
    git-ai() {
        "$GIT_AI_BINARY" "$@"
    }
    export -f git-ai

    git() {
        GIT_AI=git "$GIT_AI_BINARY" "$@"
    }
    
    export -f git
    
    # Initialize a git repo
    git init
    git config user.email "test@example.com"
    git config user.name "Test User"
    
    # Create initial commit (required for git-ai)
    echo "# Test Project" > README.md
    git add README.md
    git commit -m "Initial commit"
    git config init.defaultBranch main
    
    # Check if jq is available (needed for JSON parsing tests)
    if ! command -v jq &> /dev/null; then
        echo "WARNING: jq is not installed. JSON parsing tests may fail." >&3
        echo "Install with: sudo apt-get install jq" >&3
    fi
}

teardown() {
    # Clean up temporary directory
    cd "$ORIGINAL_DIR"
    rm -rf "$TEST_TEMP_DIR"
}

# ============================================================================
# Helper Functions
# ============================================================================

# Helper function to get clean JSON from git-ai stats
# Usage:
#   stats_json=$(get_stats_json)           # Get stats for HEAD
#   stats_json=$(get_stats_json abc123)    # Get stats for specific commit
#
# Returns: Clean JSON string without DEBUG lines
get_stats_json() {
    local commit="${1:-HEAD}"
    local stats_output
    
    # Run git-ai stats --json
    stats_output=$(git-ai stats "$commit" --json 2>&1)
    
    # Filter out DEBUG lines and extract only the JSON line
    echo "$stats_output" | grep -v '^\[DEBUG\]' | grep '^{'
}

# Helper function to verify stats JSON using jq
# Usage:
#   stats_json=$(get_stats_json)
#   verify_stats_json "$stats_json" || return 1
#
# Returns: 0 if valid JSON, 1 otherwise
verify_stats_json() {
    local stats_json="$1"
    
    # Check if jq is available
    if ! command -v jq &> /dev/null; then
        echo "SKIPPING JSON validation: jq is not installed" >&3
        echo "Install with: sudo apt-get install jq" >&3
        # At least verify the output looks like JSON
        [[ "$stats_json" =~ "ai_additions" ]] || {
            echo "ERROR: Expected JSON fields not found in output" >&3
            return 1
        }
        return 0
    fi
    
    # Verify we got JSON
    [ -n "$stats_json" ] || {
        echo "ERROR: No JSON found in stats output" >&3
        return 1
    }
    
    # Verify JSON is valid
    echo "$stats_json" | jq . >/dev/null 2>&1 || {
        echo "ERROR: Invalid JSON in stats output" >&3
        echo "Output: $stats_json" >&3
        return 1
    }
    
    return 0
}

# Helper function to compare two JSON structures
# Usage:
#   expected='{"a":1,"b":2}'
#   actual=$(get_stats_json)
#   compare_json "$expected" "$actual" "Stats mismatch" || return 1
#
# Args:
#   $1 - expected JSON string
#   $2 - actual JSON string
#   $3 - optional error message prefix
#
# Returns: 0 if JSONs match, 1 otherwise
compare_json() {
    local expected_json="$1"
    local actual_json="$2"
    local error_prefix="${3:-JSON mismatch}"
    
    # Check if jq is available
    if ! command -v jq &> /dev/null; then
        echo "WARNING: jq not available, skipping JSON comparison" >&3
        return 0
    fi
    
    # Verify both inputs are valid JSON
    if ! echo "$expected_json" | jq . >/dev/null 2>&1; then
        echo "ERROR: Expected JSON is invalid" >&3
        return 1
    fi
    
    if ! echo "$actual_json" | jq . >/dev/null 2>&1; then
        echo "ERROR: Actual JSON is invalid" >&3
        echo "Actual: $actual_json" >&3
        return 1
    fi
    
    # Canonicalize both JSONs (sort keys, compact format)
    local expected_canonical=$(echo "$expected_json" | jq -cS .)
    local actual_canonical=$(echo "$actual_json" | jq -cS .)
    
    # Compare canonicalized JSONs
    if [ "$expected_canonical" != "$actual_canonical" ]; then
        echo "ERROR: $error_prefix" >&3
        echo "" >&3
        echo "Expected (formatted):" >&3
        echo "$expected_json" | jq . >&3
        echo "" >&3
        echo "Actual (formatted):" >&3
        echo "$actual_json" | jq . >&3
        echo "" >&3
        echo "Expected (canonical): $expected_canonical" >&3
        echo "Actual (canonical):   $actual_canonical" >&3
        return 1
    fi
    
    return 0
}

# Example usage in future tests:
# @test "example test using helpers" {
#     # ... create files and commit ...
#     
#     # Get stats JSON
#     stats_json=$(get_stats_json)
#     verify_stats_json "$stats_json" || return 1
#     
#     # Compare with expected JSON
#     expected='{"ai_additions":5,"human_additions":2}'
#     compare_json "$expected" "$stats_json" "Stats verification failed" || return 1
# }

# ============================================================================
# Tests
# ============================================================================

@test "basic workflow: user creates file, AI adds code, user adds more code" {
    # Step 1: User creates a file and adds 1 line of code
    cat > example.py <<EOF
def hello():
EOF
    
    # Step 2: Checkpoint user changes
    run git-ai checkpoint
    [ "$status" -eq 0 ]
    
    # Step 3: AI adds 2 lines of code
    cat > example.py <<EOF
def hello():
    print("Hello from AI")
    return "AI generated"
EOF
    
    # Step 4: Checkpoint AI changes with mock_ai preset
    run git-ai checkpoint mock_ai example.py
    [ "$status" -eq 0 ]
    
    # Step 5: User adds 2 lines of code
    cat > example.py <<EOF
def hello():
    print("Hello from AI")
    return "AI generated"

def goodbye():
    print("Goodbye from user")
EOF
    
    # Step 6: Checkpoint user changes again
    run git-ai checkpoint
    [ "$status" -eq 0 ]
    
    # Step 7: Commit the changes
    git add example.py
    git commit -m "Add example.py with mixed authorship"
    
    # Step 8: Verify authorship with git-ai blame
    run git-ai blame example.py
    [ "$status" -eq 0 ]
    
    # Debug: show the blame output
    echo "=== Blame output ===" >&3
    echo "$output" >&3
    
    # Save output for verification
    blame_output="$output"
    
    # Verify that the output contains both user and mock_ai attributions
    [[ "$blame_output" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in blame output" >&3
        return 1
    }
    
    [[ "$blame_output" =~ "Test User" ]] || {
        echo "ERROR: 'Test User' not found in blame output" >&3
        return 1
    }
}

@test "git-ai checkpoint exits successfully without errors" {
    # Create a simple file
    echo "# Test file" > test.txt
    
    # Run checkpoint
    run git-ai checkpoint
    [ "$status" -eq 0 ]
}

@test "git-ai checkpoint mock_ai with file path" {
    # Create a file
    echo "AI generated content" > ai_file.txt
    
    # Run checkpoint with mock_ai preset
    run git-ai checkpoint mock_ai ai_file.txt
    [ "$status" -eq 0 ]
}

@test "git-ai blame shows correct attribution after commit" {
    # User creates file with one line
    echo "line1" > test.txt
    git-ai checkpoint
    
    # AI adds a line
    echo "line2" >> test.txt
    git-ai checkpoint mock_ai test.txt
    
    # Commit
    git add test.txt
    git commit -m "Test commit"
    
    # Run blame
    run git-ai blame test.txt
    [ "$status" -eq 0 ]
    
    # Output should contain the file content
    [[ "$output" =~ "line1" ]] || {
        echo "ERROR: 'line1' not found in blame output" >&3
        echo "$output" >&3
        return 1
    }
    [[ "$output" =~ "line2" ]] || {
        echo "ERROR: 'line2' not found in blame output" >&3
        echo "$output" >&3
        return 1
    }
}

@test "multiple checkpoints in sequence" {
    # Create file
    echo "step1" > multi.txt
    run git-ai checkpoint
    [ "$status" -eq 0 ]
    
    echo "step2" >> multi.txt
    run git-ai checkpoint mock_ai multi.txt
    [ "$status" -eq 0 ]
    
    echo "step3" >> multi.txt
    run git-ai checkpoint
    [ "$status" -eq 0 ]
    
    echo "step4" >> multi.txt
    run git-ai checkpoint mock_ai multi.txt
    [ "$status" -eq 0 ]


    git add multi.txt
    git commit -m "Test multiple checkpoints in sequence"

     stats_json=$(get_stats_json)
    
    # Debug: show the stats output
    echo "=== Stats JSON output ===" >&3
    echo "$stats_json" >&3

    echo "=== Blame output ===" >&3
    git-ai blame multi.txt >&3

    # Verify JSON is valid
    verify_stats_json "$stats_json" || return 1

    expected_json='{
      "human_additions": 2,
      "mixed_additions": 0,
      "ai_additions": 2,
      "ai_accepted": 2,
      "total_ai_additions": 2,
      "total_ai_deletions": 0,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 0,
      "git_diff_added_lines": 4,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 2,
          "mixed_additions": 0,
          "ai_accepted": 2,
          "total_ai_additions": 2,
          "total_ai_deletions": 0,
          "time_waiting_for_ai": 0
        }
      }
    }'

    # Compare JSONs using helper
    compare_json "$expected_json" "$stats_json" "Stats JSON does not match expected" || return 1
    
    echo "✓ Complete JSON match verified" >&3
}

@test "git-ai stats shows AI contribution after commit" {
    # User creates file
    echo "user line" > stats_test.txt
    git-ai checkpoint
    
    # AI adds content
    cat >> stats_test.txt <<EOF
AI line 1
AI line 2
AI line 3
EOF
    git-ai checkpoint mock_ai stats_test.txt
    
    # Commit
    git add stats_test.txt
    git commit -m "Test stats"
    
    # Get clean stats JSON using helper
    stats_json=$(get_stats_json)
    
    # Debug: show the stats output
    echo "=== Stats JSON output ===" >&3
    echo "$stats_json" >&3
    
    # Verify JSON is valid
    verify_stats_json "$stats_json" || return 1
    
    # Define expected JSON structure
    expected_json='{
      "human_additions": 1,
      "mixed_additions": 0,
      "ai_additions": 3,
      "ai_accepted": 3,
      "total_ai_additions": 3,
      "total_ai_deletions": 0,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 0,
      "git_diff_added_lines": 4,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 3,
          "mixed_additions": 0,
          "ai_accepted": 3,
          "total_ai_additions": 3,
          "total_ai_deletions": 0,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    # Compare JSONs using helper
    compare_json "$expected_json" "$stats_json" "Stats JSON does not match expected" || return 1
    
    echo "✓ Complete JSON match verified" >&3
}

@test "AI deletes lines from file" {
    # User creates a file with multiple lines
    cat > code.py <<EOF
def function1():
    print("Keep this")
    return 1

def function2():
    print("AI will delete this")
    return 2

def function3():
    print("Keep this too")
    return 3
EOF
    
    # Checkpoint user changes
    git-ai checkpoint
    
    # AI deletes the middle function (function2)
    cat > code.py <<EOF
def function1():
    print("Keep this")
    return 1

def function3():
    print("Keep this too")
    return 3
EOF
    
    # Checkpoint AI changes
    git-ai checkpoint mock_ai code.py
    
    # Commit
    git add code.py
    git commit -m "AI deleted function2"
    
    # Get and verify stats
    stats_json=$(get_stats_json)
    
    # Debug: show the stats output
    echo "=== Stats JSON (with deletions) ===" >&3
    echo "$stats_json" >&3
    
    # Verify JSON is valid
    verify_stats_json "$stats_json" || return 1
    
    # Define expected JSON structure for deletion scenario
    expected_json='{
      "human_additions": 7,
      "mixed_additions": 0,
      "ai_additions": 0,
      "ai_accepted": 0,
      "total_ai_additions": 0,
      "total_ai_deletions": 4,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 0,
      "git_diff_added_lines": 7,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 0,
          "mixed_additions": 0,
          "ai_accepted": 0,
          "total_ai_additions": 0,
          "total_ai_deletions": 4,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    # Compare JSONs using helper
    compare_json "$expected_json" "$stats_json" "Deletion stats JSON does not match expected" || return 1
    
    echo "✓ Complete JSON match verified (with deletions)" >&3
}

@test "Human deletes lines from AI-generated code" {
    # AI generates 5 lines of code
    cat > calculator.py <<EOF
def add(a, b):
    return a + b
def subtract(a, b):
    return a - b
def multiply(a, b):
    return a * b
EOF
    
    # Checkpoint AI changes
    git-ai checkpoint mock_ai calculator.py
    
    # Human deletes one function (subtract)
    cat > calculator.py <<EOF
def add(a, b):
    return a + b
def multiply(a, b):
    return a * b
EOF
    
    # Checkpoint human changes
    git-ai checkpoint
    
    # Commit
    git add calculator.py
    git commit -m "AI added functions, human removed one"
    
    # Get and verify stats
    stats_json=$(get_stats_json)
    
    # Debug: show the stats output
    echo "=== Stats JSON (AI code with human deletion) ===" >&3
    echo "$stats_json" >&3
    
    # Verify JSON is valid
    verify_stats_json "$stats_json" || return 1
    
    # Define expected JSON structure
    # AI generated 6 lines, human deleted 2 lines 
    expected_json='{
      "human_additions": 0,
      "mixed_additions": 0,
      "ai_additions": 4,
      "ai_accepted": 4,
      "total_ai_additions": 6,
      "total_ai_deletions": 0,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 0,
      "git_diff_added_lines": 4,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 4,
          "mixed_additions": 0,
          "ai_accepted": 4,
          "total_ai_additions": 6,
          "total_ai_deletions": 0,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    # Compare JSONs using helper
    compare_json "$expected_json" "$stats_json" "Human deletion of AI code stats mismatch" || return 1
    
    echo "✓ Stats verified: AI generated 4 lines, human deleted 2 lines, 4 accepted" >&3
}

@test "AI generates code with empty lines in between" {
    # User creates a basic file structure
    cat > app.py <<EOF
# My Application
EOF
    
    # Checkpoint user changes
    git-ai checkpoint
    
    # AI adds code with empty lines interspersed
    cat > app.py <<EOF
# My Application

import os
import sys

def setup():
    print("Setting up")

def main():
    setup()
    print("Running main")

def cleanup():
    print("Cleaning up")

if __name__ == "__main__":
    main()
EOF
    
    # Checkpoint AI changes
    git-ai checkpoint mock_ai app.py
    
    # Commit
    git add app.py
    git commit -m "AI added code with empty lines"
    
    # Get and verify stats
    stats_json=$(get_stats_json)
    
    # Debug: show the stats output
    echo "=== Stats JSON (AI code with empty lines) ===" >&3
    echo "$stats_json" >&3
    
    # Verify JSON is valid
    verify_stats_json "$stats_json" || return 1
    
    # Define expected JSON structure
    # File has 17 lines total (1 user line + 16 AI lines including empty lines)
    # AI added: 16 lines (including 4 empty lines)
    # git_diff shows 17 additions (from 1 line to 17 lines = +16)
    expected_json='{
      "human_additions": 1,
      "mixed_additions": 0,
      "ai_additions": 16,
      "ai_accepted": 16,
      "total_ai_additions": 16,
      "total_ai_deletions": 0,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 0,
      "git_diff_added_lines": 17,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 16,
          "mixed_additions": 0,
          "ai_accepted": 16,
          "total_ai_additions": 16,
          "total_ai_deletions": 0,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    git-ai blame app.py
    # Compare JSONs using helper
    compare_json "$expected_json" "$stats_json" "Empty lines tracking stats mismatch" || return 1
    
    
     echo "✓ Stats verified: AI added 16 lines including empty lines" >&3
 }

@test "AI creates a new file from scratch" {
    # No user changes - AI creates a completely new file
    
    # AI creates a new file
    cat > new_module.py <<EOF
class DataProcessor:
    def __init__(self):
        self.data = []
    def process(self, item):
        self.data.append(item)
        return item
    def get_results(self):
        return self.data
EOF
    
    # Checkpoint AI changes
    git-ai checkpoint mock_ai new_module.py
    
    # Commit
    git add new_module.py
    git commit -m "AI created new module"
    
    # Get and verify stats
    stats_json=$(get_stats_json)
    
    # Debug: show the stats output
    echo "=== Stats JSON (AI created new file) ===" >&3
    echo "$stats_json" >&3
    
    # Verify JSON is valid
    verify_stats_json "$stats_json" || return 1
    
    # Define expected JSON structure
    # AI created a file with 8 lines, all should be attributed to AI
    expected_json='{
      "human_additions": 0,
      "mixed_additions": 0,
      "ai_additions": 8,
      "ai_accepted": 8,
      "total_ai_additions": 8,
      "total_ai_deletions": 0,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 0,
      "git_diff_added_lines": 8,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 8,
          "mixed_additions": 0,
          "ai_accepted": 8,
          "total_ai_additions": 8,
          "total_ai_deletions": 0,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    # Compare JSONs using helper
    compare_json "$expected_json" "$stats_json" "New file creation stats mismatch" || return 1
    
    # Verify blame shows AI for all lines
    echo "=== Blame output (all lines should be AI) ===" >&3
    git-ai blame new_module.py >&3
    
    echo "✓ Stats verified: AI created new file with 8 lines" >&3
}


@test "squash-authorship should concatenate AI and human changes" {
    BASE_COMMIT_SHA=$(git rev-parse HEAD)
    
    # Create initial file with 5 lines
    cat > example.txt <<'EOF'
Line 1: Initial
Line 2: Initial
Line 3: Initial
Line 4: Initial
Line 5: Initial
EOF
    git add example.txt
    git commit -m "Initial file with 5 lines"
    
    # COMMIT 1: Human adds 2 lines, AI adds 3 lines, AI deletes 2 lines
    
    # Human adds lines
    cat > example.txt <<'EOF'
Line 1: Initial
Line 2: Initial
H: Human Line 1
H: Human Line 2
Line 3: Initial
Line 4: Initial
Line 5: Initial
EOF
    git-ai checkpoint  # Human added 2 lines
    
    # AI adds lines and removes some initial lines
    cat > example.txt <<'EOF'
Line 1: Initial
H: Human Line 1
H: Human Line 2
AI: AI Line 1
AI: AI Line 2
AI: AI Line 3
Line 4: Initial
Line 5: Initial
EOF
    git-ai checkpoint mock_ai example.txt  # AI added 3 lines and deleted 2 lines (Line 2, Line 3)
    
    git add example.txt
    git commit -m "Commit 1: Human adds 2, AI adds 3 and deletes 2"
    commit1_sha=$(git rev-parse HEAD)
    
    echo "=== Commit 1 Stats ===" >&3
    stats_commit1=$(get_stats_json "$commit1_sha")
    echo "$stats_commit1" >&3
    
    # Verify Commit 1 stats
    # Human added: 2 lines
    # AI added: 3 lines
    # AI deleted: 2 lines
    expected_commit1_json='{
        "human_additions": 2,
        "mixed_additions": 0,
        "ai_additions": 3,
        "ai_accepted": 3,
        "total_ai_additions": 3,
        "total_ai_deletions": 2,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 2,
        "git_diff_added_lines": 5,
        "tool_model_breakdown": {
            "mock_ai::unknown": {
                "ai_additions": 3,
                "mixed_additions": 0,
                "ai_accepted": 3,
                "total_ai_additions": 3,
                "total_ai_deletions": 2,
                "time_waiting_for_ai": 0
            }
        }
    }'
    
    compare_json "$expected_commit1_json" "$stats_commit1" "Commit 1 stats mismatch" || return 1
    echo "✓ Commit 1 stats verified" >&3
    
    # COMMIT 2: Human deletes 1 line, AI adds 2 lines and deletes 3 lines (1 human line + 1 AI line + 1 initial line)
    
    # Human deletes a line
    cat > example.txt <<'EOF'
Line 1: Initial
H: Human Line 1
H: Human Line 2
AI: AI Line 1
AI: AI Line 2
AI: AI Line 3
Line 5: Initial
EOF
    git-ai checkpoint  # Human deleted 1 line (Line 4)
    
    # AI adds more lines and removes: 1 initial line, 1 human line, 1 AI line
    cat > example.txt <<'EOF'
H: Human Line 2
AI: AI Line 1
AI: AI Line 3
AI: AI Line 4
AI: AI Line 5
Line 5: Initial
EOF
    git-ai checkpoint mock_ai example.txt  # AI added 2 lines and deleted 3 lines (Line 1: Initial, H: Human Line 1, AI: AI Line 2)
    
    git add example.txt
    git commit -m "Commit 2: Human deletes 1, AI adds 2 and deletes 3 (including 1 human line and 1 AI line)"
    commit2_sha=$(git rev-parse HEAD)
    
    echo "=== Commit 2 Stats ===" >&3
    stats_commit2=$(get_stats_json "$commit2_sha")
    echo "$stats_commit2" >&3
    
    # Verify Commit 2 stats
    # Human deleted: 1 line (Line 4: Initial)
    # AI added: 2 lines (AI Line 4, AI Line 5) - both accepted
    # AI deleted: 3 lines (Line 1: Initial, H: Human Line 1, AI: AI Line 2)
    # git_diff_deleted_lines: 4 total (1 by human checkpoint + 3 by AI checkpoint)


    expected_commit2_json='{
        "human_additions": 0,
        "mixed_additions": 0,
        "ai_additions": 2,
        "ai_accepted": 2,
        "total_ai_additions": 2,
        "total_ai_deletions": 3,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 4,
        "git_diff_added_lines": 2,
        "tool_model_breakdown": {
            "mock_ai::unknown": {
                "ai_additions": 2,
                "mixed_additions": 0,
                "ai_accepted": 2,
                "total_ai_additions": 2,
                "total_ai_deletions": 3,
                "time_waiting_for_ai": 0
            }
        }
    }'
    
    compare_json "$expected_commit2_json" "$stats_commit2" "Commit 2 stats mismatch" || return 1
    echo "✓ Commit 2 stats verified" >&3

    # Capture blame output BEFORE squashing (from commit 2)
    echo "=== Blame BEFORE squash (from commit 2) ===" >&3
    blame_before_squash=$(git-ai blame example.txt)
    echo "$blame_before_squash" >&3
    
    ## Squash the two commits
    git checkout -b squashed-branch "$BASE_COMMIT_SHA"

    # Squash the last 2 commits into one using merge --squash
    git merge --squash "$commit2_sha"
    git commit -m "Squashed: Combined changes from both commits"
    
    squashed_commit_sha=$(git rev-parse HEAD)
    
    # Now run squash-authorship to merge the authorship logs
    echo "=== Running squash-authorship ===" >&3
    git-ai squash-authorship squashed-branch "$squashed_commit_sha" "$commit2_sha"

    echo "=== Blame AFTER squash-authorship ===" >&3
    blame_after_squash=$(git-ai blame example.txt)
    echo "$blame_after_squash" >&3
    
    # Verify blame outputs are identical (line attributions preserved)
    # Extract just the author and line content (ignore commit SHAs and timestamps)
    blame_before_lines=$(echo "$blame_before_squash" | awk '{print $2, $NF}' | sort)
    blame_after_lines=$(echo "$blame_after_squash" | awk '{print $2, $NF}' | sort)
    
    if [ "$blame_before_lines" != "$blame_after_lines" ]; then
        echo "ERROR: Blame attributions changed after squash!" >&3
        echo "Before:" >&3
        echo "$blame_before_lines" >&3
        echo "After:" >&3
        echo "$blame_after_lines" >&3
        return 1
    fi
    
    echo "✓ Blame attributions preserved after squash" >&3
    
    # Get squashed stats
    stats_squashed=$(get_stats_json "$squashed_commit_sha")
    echo "=== Squashed Stats ===" >&3
    echo "$stats_squashed" >&3
    
    # Verify the squashed commit has combined authorship from both commits
    # After squash-authorship concatenates the logs:
    # Final file lines (6 total):
    #   1. H: Human Line 2 (human-attributed)
    #   2. AI: AI Line 1 (AI-attributed)
    #   3. AI: AI Line 3 (AI-attributed)
    #   4. AI: AI Line 4 (AI-attributed)
    #   5. AI: AI Line 5 (AI-attributed)
    #   6. Line 5: Initial (human-attributed, was in original)
    # 
    # The squashed commit shows ACCUMULATED stats from both commits:
    # - human_additions: 2 (H: Human Line 2 + Line 5: Initial in final diff)
    # - ai_additions: 4 (AI Line 1, 3, 4, 5 in final diff)
    # - ai_accepted: 4 (all AI lines accepted)
    # - total_ai_additions: 5 (3 from commit 1 + 2 from commit 2)
    # - total_ai_deletions: 5 (2 from commit 1 + 3 from commit 2)
    # - tool_model_breakdown.ai_additions: 4 (accepted + mixed in final diff)

    expected_json='{
        "human_additions": 2,
        "mixed_additions": 0,
        "ai_additions": 4,
        "ai_accepted": 4,
        "total_ai_additions": 5,
        "total_ai_deletions": 5,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 0,
        "git_diff_added_lines": 6,
        "tool_model_breakdown": {
            "mock_ai::unknown": {
                "ai_additions": 4,
                "mixed_additions": 0,
                "ai_accepted": 4,
                "total_ai_additions": 5,
                "total_ai_deletions": 5,
                "time_waiting_for_ai": 0
            }
        }
    }'
    
    compare_json "$expected_json" "$stats_squashed" "Squashed stats mismatch" || return 1
    
    # Final verification: ensure both AI and human attributions exist
    [[ "$blame_after_squash" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in squashed blame output" >&3
        return 1
    }
    
    [[ "$blame_after_squash" =~ "Test User" ]] || {
        echo "ERROR: 'Test User' not found in squashed blame output" >&3
        return 1
    }
    
    echo "✓ Squash-authorship successfully preserved line attributions and merged logs" >&3
    echo "  - Blame before and after squash: IDENTICAL ✓" >&3
    echo "  - Final: 2 human lines, 4 AI lines" >&3
    echo "  - AI deleted 1 human line and 1 AI line during development" >&3
}

@test "AI refactors its own code - squash-authorship should show no ai_deletions" {
    skip "https://github.com/git-ai-project/git-ai/issues/162"
    touch fibonacci.ts
    git add fibonacci.ts
    git commit -m "Initial empty file"
    
    # COMMIT 1: AI creates initial iterative implementation
    cat > fibonacci.ts <<'EOF'
export const fibonacci = (n: number): number => {
  if (n <= 0) return 0;
  if (n === 1) return 1;

  let prev = 0;
  let curr = 1;

  for (let i = 2; i <= n; i++) {
    const next = prev + curr;
    prev = curr;
    curr = next;
  }

  return curr;
}
EOF
    git-ai checkpoint mock_ai fibonacci.ts  # AI created the function
    
    git add fibonacci.ts
    git commit -m "Commit 1: AI creates fibonacci"
    
    # Get stats for commit 1
    commit1_sha=$(git rev-parse HEAD)
    stats_commit1=$(get_stats_json "$commit1_sha")
    echo "=== Commit 1 Stats ===" >&3
    echo "$stats_commit1" >&3
    
    # COMMIT 2: AI refactors to recursive implementation (deletes most of commit 1)
    cat > fibonacci.ts <<'EOF'
export const fibonacci = (n: number): number => {
  console.log('executing fibonacci');
  if (n <= 0) return 0;
  if (n === 1) return 1;
  return fibonacci(n - 1) + fibonacci(n - 2);
}
EOF
    
    git-ai checkpoint mock_ai fibonacci.ts  # AI refactored
    
    git add fibonacci.ts
    git commit -m "Commit 2: AI refactors to recursive"
    
    # Get stats for commit 2
    commit2_sha=$(git rev-parse HEAD)
    stats_commit2=$(get_stats_json "$commit2_sha")
    echo "=== Commit 2 Stats ===" >&3
    echo "$stats_commit2" >&3

    
    git checkout -b squashed-branch

    # Squash the last 2 commits into one
    git reset --soft HEAD~2
    git commit -m "Squashed: AI creates iterative then refactors to recursive fibonacci"
    
    new_commit_sha=$(git rev-parse HEAD)
    # Now squash authorship
    echo "=== Running squash-authorship ===" >&3
    git-ai squash-authorship main "$new_commit_sha" "$commit2_sha"

    git-ai blame fibonacci.ts
    git diff "$new_commit_sha"
    
    
    # Get squashed stats
    stats_squashed=$(get_stats_json "$new_commit_sha")
    echo "=== Squashed Stats ===" >&3
    echo "$stats_squashed" >&3
    
    expected_json='{
      "human_additions": 0,
      "ai_additions": 6,
      "ai_deletions": 0,
      "git_diff_added_lines": 6,
      "git_diff_deleted_lines": 0,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 6,
          "mixed_additions": 0,
          "ai_accepted": 6,
          "ai_deletions": 0,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    compare_json "$expected_json" "$stats_squashed" "Squashed stats mismatch" || return 1
    
    echo "✓ Stats verified: Squashed stats match expected" >&3
}

@test "Two AI commits, reset last commit, then recommit" {
    skip "https://github.com/git-ai-project/git-ai/issues/169"
    # COMMIT 1: AI creates first file
    cat > module1.py <<EOF
def function_one():
    print("AI generated function 1")
    return 1
EOF
    
    git-ai checkpoint mock_ai module1.py
    git add module1.py
    git commit -m "Commit 1: AI creates module1.py"
    
    # Get stats for commit 1
    commit1_sha=$(git rev-parse HEAD)
    stats_commit1=$(get_stats_json "$commit1_sha")
    echo "=== Commit 1 Stats ===" >&3
    echo "$stats_commit1" >&3
    
    # Verify commit 1 stats
    verify_stats_json "$stats_commit1" || return 1
    
    # COMMIT 2: AI creates second file
    cat > module2.py <<EOF
def function_two():
    print("AI generated function 2")
    return 2
def helper():
    return "helper"
EOF
    
    git-ai checkpoint mock_ai module2.py
    git add module2.py
    git commit -m "Commit 2: AI creates module2.py"
    
    # Get stats for commit 2 (before reset)
    commit2_sha=$(git rev-parse HEAD)
    stats_commit2_before=$(get_stats_json "$commit2_sha")
    echo "=== Commit 2 Stats (before reset) ===" >&3
    echo "$stats_commit2_before" >&3
    
    # Verify commit 2 stats before reset
    verify_stats_json "$stats_commit2_before" || return 1
    
    # Expected stats for commit 2 before reset
    expected_commit2_json='{
      "human_additions": 0,
      "mixed_additions": 0,
      "ai_additions": 5,
      "ai_accepted": 5,
      "total_ai_additions": 5,
      "total_ai_deletions": 0,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 0,
      "git_diff_added_lines": 5,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 5,
          "mixed_additions": 0,
          "ai_accepted": 5,
          "total_ai_additions": 5,
          "total_ai_deletions": 0,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    compare_json "$expected_commit2_json" "$stats_commit2_before" "Commit 2 stats before reset mismatch" || return 1
    
    # RESET: Reset the last commit (soft reset keeps working directory and index)
    echo "=== Resetting last commit ===" >&3
    git reset --soft HEAD~1
    
    # Verify we're back to commit 1
    current_sha=$(git rev-parse HEAD)
    [ "$current_sha" = "$commit1_sha" ] || {
        echo "ERROR: After reset, HEAD should be at commit 1" >&3
        return 1
    }
    
    # Verify module2.py is still staged (soft reset keeps index)
    git status --short >&3
    
    # RECOMMIT: Commit the changes again
    git commit -m "Commit 2 (recommitted): AI creates module2.py"
    
    # Get stats for the recommitted commit
    commit2_new_sha=$(git rev-parse HEAD)
    stats_commit2_after=$(get_stats_json "$commit2_new_sha")
    echo "=== Commit 2 Stats (after reset and recommit) ===" >&3
    echo "$stats_commit2_after" >&3
    
    # Verify commit 2 stats after recommit
    verify_stats_json "$stats_commit2_after" || return 1
    
    # The stats should be the same as before the reset
    compare_json "$expected_commit2_json" "$stats_commit2_after" "Commit 2 stats after recommit mismatch" || return 1
    
    # Verify blame still works correctly for both files
    echo "=== Blame for module1.py ===" >&3
    git-ai blame module1.py >&3
    
    echo "=== Blame for module2.py ===" >&3
    git-ai blame module2.py >&3
    
    # Verify both files show mock_ai attribution
    blame_output1=$(git-ai blame module1.py)
    blame_output2=$(git-ai blame module2.py)
    
    [[ "$blame_output1" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in module1.py blame output" >&3
        return 1
    }
    
    [[ "$blame_output2" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in module2.py blame output" >&3
        return 1
    }
    
    echo "✓ Stats verified: Authorship tracking persists after reset and recommit" >&3
}

@test "AI authorship is preserved after rebase" {
    # Step 1: Create initial state on main branch
    cat > base.py <<EOF
# Base module
def base_function():
    return "base"
EOF
    git-ai checkpoint
    git add base.py
    git commit -m "Initial base file"
    
    # Step 2: Create a feature branch
    git checkout -b feature-branch
    
    # Step 3: AI creates a file on feature branch
    cat > feature.py <<EOF
def ai_feature():
    print("AI generated feature")
    return "feature"
class AIHelper:
    def __init__(self):
        self.name = "AI Helper"
    def help(self):
        return "AI assistance"
EOF
    
    git-ai checkpoint mock_ai feature.py
    git add feature.py
    git commit -m "AI creates feature module"
    
    # Get stats for the feature commit before rebase
    feature_commit_before=$(git rev-parse HEAD)
    stats_before=$(get_stats_json "$feature_commit_before")
    
    echo "=== Stats BEFORE rebase ===" >&3
    echo "$stats_before" >&3
    
    # Verify stats before rebase
    verify_stats_json "$stats_before" || return 1
    
    # Verify AI authorship before rebase
    echo "=== Blame BEFORE rebase ===" >&3
    git-ai blame feature.py >&3
    
    blame_before=$(git-ai blame feature.py)
    [[ "$blame_before" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in blame output before rebase" >&3
        return 1
    }

    expected_json='{
      "human_additions": 0,
      "mixed_additions": 0,
      "ai_additions": 8,
      "ai_accepted": 8,
      "total_ai_additions": 8,
      "total_ai_deletions": 0,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 0,
      "git_diff_added_lines": 8,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 8,
          "mixed_additions": 0,
          "ai_accepted": 8,
          "total_ai_additions": 8,
          "total_ai_deletions": 0,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    compare_json "$expected_json" "$stats_before" "Stats before rebase do not match expected" || return 1
    
    # Step 4: Switch back to main and create a new commit
    git checkout main
    cat >> base.py <<EOF

def new_base_function():
    return "new base"
EOF
    git-ai checkpoint
    git add base.py
    git commit -m "Add new function to base"
    
    # Step 5: Rebase feature branch onto updated main
    echo "=== Performing rebase ===" >&3
    git checkout feature-branch
    git rebase main
    
    # Step 6: Verify AI authorship is preserved after rebase
    echo "=== Stats AFTER rebase ===" >&3
    feature_commit_after=$(git rev-parse HEAD)
    stats_after=$(get_stats_json "$feature_commit_after")
    echo "$stats_after" >&3
    
    # Verify stats after rebase
    verify_stats_json "$stats_after" || return 1
    
    # Expected stats should match the original (AI created 8 lines)
    # The key verification is that stats remain identical before and after rebase
    
    compare_json "$expected_json" "$stats_after" "Stats after rebase do not match expected" || return 1
    
    # Verify the file still exists and has the expected content
    [ -f feature.py ] || {
        echo "ERROR: feature.py not found after rebase" >&3
        return 1
    }
    
    # Verify content integrity
    grep -q "ai_feature" feature.py || {
        echo "ERROR: Expected content not found in feature.py after rebase" >&3
        return 1
    }
    
    echo "✓ AI authorship successfully preserved after rebase" >&3
}


@test "AI attribution is preserved after fixing conflict during rebase" {
    # Step 1: Create initial file on main branch
    cat > shared.py <<EOF
def function_one():
    return 1
def function_two():
    return 2
EOF
    git-ai checkpoint
    git add shared.py
    git commit -m "Initial shared file"
    
    # Step 2: Create feature branch where AI modifies the file
    git checkout -b feature-ai
    
    # AI modifies function_two and adds new content
    cat > shared.py <<EOF
def function_one():
    return 1
def function_two():
    # AI enhanced this function
    result = 2 * 2
    return result
def ai_function():
    print("AI added this")
    return "ai_data"
EOF
    
    git-ai checkpoint mock_ai shared.py
    git add shared.py
    git commit -m "AI enhances function_two and adds ai_function"
    
    # Get stats before conflict resolution
    ai_stats_before=$(get_stats_json "$(git rev-parse HEAD)")
    echo "=== AI Stats BEFORE conflict resolution ===" >&3
    echo "$ai_stats_before" >&3
    
    verify_stats_json "$ai_stats_before" || return 1
    
    # Verify AI authorship before conflict
    git-ai blame shared.py >&3
    
    blame_before=$(git-ai blame shared.py)
    [[ "$blame_before" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in blame output before conflict" >&3
        return 1
    }

    expected_json='{
        "human_additions": 0,
        "mixed_additions": 0,
        "ai_additions": 6,
        "ai_accepted": 6,
        "total_ai_additions": 6,
        "total_ai_deletions": 1,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 1,
        "git_diff_added_lines": 6,
        "tool_model_breakdown": {
            "mock_ai::unknown": {
            "ai_additions": 6,
            "mixed_additions": 0,
            "ai_accepted": 6,
            "total_ai_additions": 6,
            "total_ai_deletions": 1,
            "time_waiting_for_ai": 0
            }
        }
    }'

    compare_json "$expected_json" "$ai_stats_before" "AI Stats before conflict resolution do not match expected" || return 1
    
    # Step 3: Go back to main and make conflicting changes
    git checkout main
    
    # Human modifies function_two differently (will cause conflict)
    cat > shared.py <<EOF
def function_one():
    return 1
def function_two():
    # Human modified this differently
    value = 2 + 2
    return value
def human_function():
    return "human_data"
EOF
    
    git-ai checkpoint
    git add shared.py
    git commit -m "Human modifies function_two and adds human_function"

    echo "=== Human Stats BEFORE conflict resolution ===" >&3
    git-ai blame shared.py >&3
    human_stats_before=$(get_stats_json "$(git rev-parse HEAD)")
    echo "$human_stats_before" >&3
    verify_stats_json "$human_stats_before" || return 1

    expected_json='{
        "human_additions": 5,
        "mixed_additions": 0,
        "ai_additions": 0,
        "ai_accepted": 0,
        "total_ai_additions": 0,
        "total_ai_deletions": 0,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 1,
        "git_diff_added_lines": 5,
        "tool_model_breakdown": {}
    }'

    compare_json "$expected_json" "$human_stats_before" "Human Stats before conflict do not match expected" || return 1
    
    # Step 4: Attempt rebase - this will cause a conflict
    git checkout feature-ai
    echo "=== Attempting rebase (will conflict) ===" >&3
    
    # Rebase will stop due to conflict
    if git rebase main 2>&3; then
        echo "ERROR: Expected rebase to fail with conflict, but it succeeded" >&3
        return 1
    fi
    
    # Verify we're in a conflicted state
    git status >&3
    
    # Step 5: Resolve the conflict by keeping both changes
    cat > shared.py <<EOF
def function_one():
    return 1
def function_two():
    # AI enhanced this function
    result = 2 * 2
    return result
def ai_function():
    print("AI added this")
    return "ai_data"
def human_function():
    return "human_data"
EOF
    
    # Mark conflict as resolved
    git add shared.py
    
    # Continue rebase (set GIT_EDITOR to bypass interactive editor)
    echo "=== Continuing rebase after conflict resolution ===" >&3
    GIT_EDITOR=true git rebase --continue
    
    # Step 6: Verify AI authorship is preserved after conflict resolution
    echo "=== Stats AFTER conflict resolution ===" >&3
    feature_commit_after=$(git rev-parse HEAD)
    stats_after=$(get_stats_json "$feature_commit_after")
    echo "$stats_after" >&3
    
    verify_stats_json "$stats_after" || return 1
    
    # The stats should show AI additions preserved after conflict resolution
    # AI added 6 lines (function_two enhancement: 3 lines, ai_function: 3 lines)
    # Note: The conflict resolution shows deletions from both human and AI sides
    expected_json='{
      "human_additions": 0,
      "mixed_additions": 0,
      "ai_additions": 6,
      "ai_accepted": 6,
      "total_ai_additions": 6,
      "total_ai_deletions": 1,
      "time_waiting_for_ai": 0,
      "git_diff_deleted_lines": 3,
      "git_diff_added_lines": 6,
      "tool_model_breakdown": {
        "mock_ai::unknown": {
          "ai_additions": 6,
          "mixed_additions": 0,
          "ai_accepted": 6,
          "total_ai_additions": 6,
          "total_ai_deletions": 1,
          "time_waiting_for_ai": 0
        }
      }
    }'
    
    compare_json "$expected_json" "$stats_after" "Stats after conflict resolution do not match expected" || return 1
    
    # Verify blame shows AI authorship for AI lines
    echo "=== Blame AFTER conflict resolution ===" >&3
    git-ai blame shared.py >&3
    # Show the diff for informational purposes
    echo "=== Git Diff after conflict resolution ===" >&3
    git diff HEAD^ HEAD -- shared.py >&3
    
    blame_after=$(git-ai blame shared.py)
    
    # Verify AI attribution is present
    [[ "$blame_after" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in blame output after conflict resolution" >&3
        echo "AI authorship was NOT preserved after conflict resolution!" >&3
        return 1
    }
    
    # Verify human attribution is also present
    [[ "$blame_after" =~ "Test User" ]] || {
        echo "ERROR: 'Test User' not found in blame output after conflict resolution" >&3
        return 1
    }
    
    # Verify the file has expected content
    grep -q "AI added this" shared.py || {
        echo "ERROR: AI content not found in resolved file" >&3
        return 1
    }
    
    grep -q "human_data" shared.py || {
        echo "ERROR: Human content not found in resolved file" >&3
        return 1
    }
    
    echo "✓ AI attribution successfully preserved after conflict resolution during rebase" >&3
}

@test "git-ai stats range command works correctly" {
    # Get the base commit (initial commit from setup)
    base_commit=$(git rev-parse HEAD)
    
    # COMMIT 1: Human adds 3 lines to a file
    cat > example.txt <<'EOF'
H: Human Line 1
H: Human Line 2
H: Human Line 3
EOF
    
    git-ai checkpoint
    git add example.txt
    git commit -m "Commit 1: Human adds 3 lines"
    
    commit1_sha=$(git rev-parse HEAD)
    
    echo "=== Commit 1 SHA: $commit1_sha ===" >&3
    
    # COMMIT 2: AI adds 5 more lines to the same file
    cat > example.txt <<'EOF'
H: Human Line 1
H: Human Line 2
H: Human Line 3
AI: AI Line 1
AI: AI Line 2
AI: AI Line 3
AI: AI Line 4
AI: AI Line 5
EOF
    
    git-ai checkpoint mock_ai example.txt
    git add example.txt
    git commit -m "Commit 2: AI adds 5 lines"
    
    commit2_sha=$(git rev-parse HEAD)
    
    echo "=== Commit 2 SHA: $commit2_sha ===" >&3
    
    # Test the range command: base_commit..commit2_sha
    # This should include both commit1 and commit2
    echo "=== Testing range: $base_commit..$commit2_sha ===" >&3
    
    stats_output=$(git-ai stats "$base_commit..$commit2_sha" --json 2>&1)
    stats_json=$(echo "$stats_output" | grep -v '^\[DEBUG\]' | grep '^{')
    
    echo "=== Stats JSON for range ===" >&3
    echo "$stats_json" >&3
    
    # Verify JSON is valid
    if ! command -v jq &> /dev/null; then
        echo "WARNING: jq not available, skipping JSON validation" >&3
    else
        echo "$stats_json" | jq . >/dev/null 2>&1 || {
            echo "ERROR: Invalid JSON in stats output" >&3
            return 1
        }
    fi
    
    # Expected stats for the range (both commits combined):
    # - Commit 1: Human added 3 lines (module1.py)
    # - Commit 2: AI added 5 lines (module2.py)
    # Total: 3 human additions, 5 AI additions
    # Note: Range stats are nested under "range_stats" field
    expected_json='{
      "authorship_stats": {
        "total_commits": 2,
        "commits_with_authorship": 2,
        "authors_committing_authorship": ["Test User <test@example.com>"],
        "authors_not_committing_authorship": [],
        "commits_without_authorship": [],
        "commits_without_authorship_with_authors": []
      },
      "range_stats": {
        "human_additions": 3,
        "mixed_additions": 0,
        "ai_additions": 5,
        "ai_accepted": 5,
        "total_ai_additions": 5,
        "total_ai_deletions": 0,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 0,
        "git_diff_added_lines": 8,
        "tool_model_breakdown": {
          "mock_ai::unknown": {
            "ai_additions": 5,
            "mixed_additions": 0,
            "ai_accepted": 5,
            "total_ai_additions": 5,
            "total_ai_deletions": 0,
            "time_waiting_for_ai": 0
          }
        }
      }
    }'
    
    compare_json "$expected_json" "$stats_json" "Range stats do not match expected" || return 1
    
    # Also test with just the second commit in the range
    echo "=== Testing range: $commit1_sha..$commit2_sha ===" >&3
    
    stats_output_single=$(git-ai stats "$commit1_sha..$commit2_sha" --json 2>&1)
    stats_json_single=$(echo "$stats_output_single" | grep -v '^\[DEBUG\]' | grep '^{')
    
    echo "=== Stats JSON for single commit range ===" >&3
    echo "$stats_json_single" >&3
    
    # Verify JSON is valid
    if ! command -v jq &> /dev/null; then
        echo "WARNING: jq not available, skipping JSON validation" >&3
    else
        echo "$stats_json_single" | jq . >/dev/null 2>&1 || {
            echo "ERROR: Invalid JSON in stats output" >&3
            return 1
        }
    fi
    
    # Expected stats for commit1..commit2 (only commit2):
    # - Only Commit 2: AI added 5 lines (module2.py)
    expected_json_single='{
      "authorship_stats": {
        "total_commits": 1,
        "commits_with_authorship": 1,
        "authors_committing_authorship": ["Test User <test@example.com>"],
        "authors_not_committing_authorship": [],
        "commits_without_authorship": [],
        "commits_without_authorship_with_authors": []
      },
      "range_stats": {
        "human_additions": 0,
        "mixed_additions": 0,
        "ai_additions": 5,
        "ai_accepted": 5,
        "total_ai_additions": 5,
        "total_ai_deletions": 0,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 0,
        "git_diff_added_lines": 5,
        "tool_model_breakdown": {
          "mock_ai::unknown": {
            "ai_additions": 5,
            "mixed_additions": 0,
            "ai_accepted": 5,
            "total_ai_additions": 5,
            "total_ai_deletions": 0,
            "time_waiting_for_ai": 0
          }
        }
      }
    }'
    
    compare_json "$expected_json_single" "$stats_json_single" "Single commit range stats do not match expected" || return 1
    
    echo "✓ Stats range command verified successfully" >&3
}

@test "interactive rebase with squash preserves authorship" {
    # Create initial file with base API endpoint structure
    cat > api_handler.py <<'EOF'
from flask import Flask, request, jsonify

app = Flask(__name__)

# API endpoint placeholder
EOF
    git add api_handler.py
    git commit -m "Base commit with initial API structure"
    
    # Remember the base commit for validation
    base_commit_sha=$(git rev-parse HEAD)
    
    # COMMIT 1: Human adds 2 lines, AI adds 3 lines
    
    # Human adds route definition and error handling
    cat > api_handler.py <<'EOF'
from flask import Flask, request, jsonify

app = Flask(__name__)

@app.route('/api/users', methods=['POST'])
def create_user():
# API endpoint placeholder
EOF
    git-ai checkpoint  # Human added 2 lines
    
    # AI adds data extraction and basic validation
    cat > api_handler.py <<'EOF'
from flask import Flask, request, jsonify

app = Flask(__name__)

@app.route('/api/users', methods=['POST'])
def create_user():
    data = request.get_json()
    username = data.get('username', '') if data else ''
    return jsonify({'user': username}), 201
# API endpoint placeholder
EOF
    git-ai checkpoint mock_ai api_handler.py  # AI added 3 lines
    
    git add api_handler.py
    git commit -m "Commit 1: Add user creation endpoint with basic implementation"
    commit1_sha=$(git rev-parse HEAD)
    
    echo "=== Commit 1 Stats ===" >&3
    stats_commit1=$(get_stats_json "$commit1_sha")
    echo "$stats_commit1" >&3
    
    # Verify Commit 1 stats
    expected_commit1_json='{
        "human_additions": 2,
        "mixed_additions": 0,
        "ai_additions": 3,
        "ai_accepted": 3,
        "total_ai_additions": 3,
        "total_ai_deletions": 0,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 0,
        "git_diff_added_lines": 5,
        "tool_model_breakdown": {
            "mock_ai::unknown": {
                "ai_additions": 3,
                "mixed_additions": 0,
                "ai_accepted": 3,
                "total_ai_additions": 3,
                "total_ai_deletions": 0,
                "time_waiting_for_ai": 0
            }
        }
    }'
    
    compare_json "$expected_commit1_json" "$stats_commit1" "Commit 1 stats mismatch" || return 1
    echo "✓ Commit 1 stats verified" >&3
    
    # COMMIT 2: Human adds 2 lines, AI deletes 1 of its own AI lines and adds 2 lines
    
    # Human adds documentation comments
    cat > api_handler.py <<'EOF'
from flask import Flask, request, jsonify

app = Flask(__name__)

@app.route('/api/users', methods=['POST'])
def create_user():
    data = request.get_json()
    username = data.get('username', '') if data else ''
    return jsonify({'user': username}), 201
    # TODO: Add proper database integration
    # TODO: Add authentication check
# API endpoint placeholder
EOF
    git-ai checkpoint  # Human added 2 lines
    
    # AI improves validation (removes simple return, adds validation logic)
    cat > api_handler.py <<'EOF'
from flask import Flask, request, jsonify

app = Flask(__name__)

@app.route('/api/users', methods=['POST'])
def create_user():
    data = request.get_json()
    username = data.get('username', '') if data else ''
    # TODO: Add proper database integration
    # TODO: Add authentication check
    if not username or len(username) < 3:
        return jsonify({'error': 'Invalid username'}), 400
# API endpoint placeholder
EOF
    git-ai checkpoint mock_ai api_handler.py  # AI deleted 1 line and added 2 lines
    
    git add api_handler.py
    git commit -m "Commit 2: Add documentation and improve validation"
    commit2_sha=$(git rev-parse HEAD)
    
    echo "=== Commit 2 Stats ===" >&3
    stats_commit2=$(get_stats_json "$commit2_sha")
    echo "$stats_commit2" >&3
    
    # Verify Commit 2 stats
    # Human added 2 lines
    # AI added 2 new lines
    # AI deleted 1 of its own AI lines from Commit 1
    # Expectations:
    #  - human_additions: 2 (2 human lines added)
    #  - ai_additions: 2 (2 new AI lines added)
    #  - ai_accepted: 2 (2 new lines are not modified by human)
    #  - mixed_additions: 0 (AI lines are not modified by human)
    #  - total_ai_additions: 2 (AI added 2 new lines)
    #  - total_ai_deletions: 1 (AI deleted one of its previous lines)
    #  - git_diff_deleted_lines: 1 (deleted 1 AI line)
    #  - git_diff_added_lines: 4 (2 human + 2 AI lines added)
    expected_commit2_json='{
        "human_additions": 2,
        "mixed_additions": 0,
        "ai_additions": 2,
        "ai_accepted": 2,
        "total_ai_additions": 2,
        "total_ai_deletions": 1,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 1,
        "git_diff_added_lines": 4,
        "tool_model_breakdown": {
            "mock_ai::unknown": {
                "ai_additions": 2,
                "mixed_additions": 0,
                "ai_accepted": 2,
                "total_ai_additions": 2,
                "total_ai_deletions": 1,
                "time_waiting_for_ai": 0
            }
        }
    }'
    
    compare_json "$expected_commit2_json" "$stats_commit2" "Commit 2 stats mismatch" || return 1
    echo "✓ Commit 2 stats verified" >&3
    
    # Capture blame output BEFORE squashing
    echo "=== Blame BEFORE squash ===" >&3
    blame_before_squash=$(git-ai blame api_handler.py)
    echo "$blame_before_squash" >&3
    
    # Perform interactive rebase to squash the last 2 commits
    echo "=== Performing interactive rebase with squash ===" >&3
    
    # Use the squash-editor.sh script from the repository
    export GIT_SEQUENCE_EDITOR="$ORIGINAL_DIR/tests/e2e/squash-editor.sh"
    export GIT_EDITOR="echo 'Squashed: Implement user creation endpoint with validation and logging' >"
    
    # Perform the interactive rebase
    git rebase -i --autosquash HEAD~2 2>&1
    
    unset GIT_SEQUENCE_EDITOR
    unset GIT_EDITOR
    
    # Verify that the rebase resulted in one commit
    squashed_commit_sha=$(git rev-parse HEAD)
    new_commit_count=$(git rev-list --count HEAD ^$base_commit_sha)
    
    if [[ "$new_commit_count" != "1" ]]; then
        echo "ERROR: Interactive rebase did not result in one commit (got $new_commit_count)" >&3
        git log --oneline HEAD~3..HEAD >&3
        return 1
    fi
    
    echo "✓ Successfully squashed last 2 commits into one" >&3
    
    # Get squashed commit stats
    echo "=== Squashed Commit Stats ===" >&3
    stats_squashed=$(get_stats_json "$squashed_commit_sha")
    echo "$stats_squashed" >&3
    
    # Verify blame output AFTER squashing
    echo "=== Blame AFTER squash ===" >&3
    blame_after_squash=$(git-ai blame api_handler.py)
    echo "$blame_after_squash" >&3
    
    # Verify that both AI and human attributions are preserved
    [[ "$blame_after_squash" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in squashed blame output" >&3
        return 1
    }
    
    [[ "$blame_after_squash" =~ "Test User" ]] || {
        echo "ERROR: 'Test User' not found in squashed blame output" >&3
        return 1
    }
    
    # Verify the squashed commit has combined authorship from all commits
    # Final file should have (compared to base):
    # - 4 human lines (2 from commit 1: route + function, 2 from commit 2: TODO comments)
    # - 4 AI lines in final file (2 from commit 1: data extraction + username parsing, 2 from commit 2: validation)
    # Total git diff shows: 8 additions (4 human + 4 AI)
    # Total AI additions across all commits: 3 + 2 = 5 (including 1 line later deleted by AI)
    # Total AI deletions across all commits: 1 (AI replaced simple return with validation)
    
    expected_squashed_json='{
        "human_additions": 4,
        "mixed_additions": 0,
        "ai_additions": 4,
        "ai_accepted": 4,
        "total_ai_additions": 5,
        "total_ai_deletions": 1,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 0,
        "git_diff_added_lines": 8,
        "tool_model_breakdown": {
            "mock_ai::unknown": {
                "ai_additions": 4,
                "mixed_additions": 0,
                "ai_accepted": 4,
                "total_ai_additions": 5,
                "total_ai_deletions": 1,
                "time_waiting_for_ai": 0
            }
        }
    }'
    
    compare_json "$expected_squashed_json" "$stats_squashed" "Squashed stats mismatch" || return 1
    
    echo "✓ Interactive rebase with squash successfully preserved line attributions" >&3
}

@test "rebase feature branch with mixed authorship onto diverged main" {
    # Step 1: Create initial state on main branch (common ancestor)
    # Create a file with clearly separated sections
    cat > app.py <<'EOF'
# Application Module
# This file contains the main application logic

def main():
    print("Application starting")

# Utility functions section
# Add utility functions below

# Data processing section  
# Add data processing functions below

# End of file
EOF
    git add app.py
    git commit -m "Initial application setup"
    
    # Remember the common ancestor commit
    common_ancestor=$(git rev-parse HEAD)
    echo "=== Common ancestor: $common_ancestor ===" >&3
    
    # Step 2: Create feature branch (both branches have same head)
    git checkout -b feature
    
    # Step 3: Add commit with mixed AI and Human authorship on feature branch
    # Feature will add content in the "Data processing section"
    # Human adds the function signature and setup
    cat > app.py <<'EOF'
# Application Module
# This file contains the main application logic

def main():
    print("Application starting")

# Utility functions section
# Add utility functions below

# Data processing section  
# Add data processing functions below

def process_data(input_data):
    # Validate input

# End of file
EOF
    git-ai checkpoint  # Human added 3 lines
    
    # AI adds the implementation logic
    cat > app.py <<'EOF'
# Application Module
# This file contains the main application logic

def main():
    print("Application starting")

# Utility functions section
# Add utility functions below

# Data processing section  
# Add data processing functions below

def process_data(input_data):
    # Validate input
    if not input_data:
        return None
    result = input_data.upper()
    return result

# End of file
EOF
    git-ai checkpoint mock_ai app.py  # AI added 4 lines
    
    git add app.py
    git commit -m "Feature: Add data processing function"
    
    feature_commit_before=$(git rev-parse HEAD)
    
    echo "=== Feature commit stats BEFORE rebase ===" >&3
    stats_feature_before=$(get_stats_json "$feature_commit_before")
    echo "$stats_feature_before" >&3
    
    # Verify the feature commit has mixed authorship
    expected_feature_json='{
        "human_additions": 3,
        "mixed_additions": 0,
        "ai_additions": 4,
        "ai_accepted": 4,
        "total_ai_additions": 4,
        "total_ai_deletions": 0,
        "time_waiting_for_ai": 0,
        "git_diff_deleted_lines": 0,
        "git_diff_added_lines": 7,
        "tool_model_breakdown": {
            "mock_ai::unknown": {
                "ai_additions": 4,
                "mixed_additions": 0,
                "ai_accepted": 4,
                "total_ai_additions": 4,
                "total_ai_deletions": 0,
                "time_waiting_for_ai": 0
            }
        }
    }'
    
    compare_json "$expected_feature_json" "$stats_feature_before" "Feature commit stats do not match expected" || return 1
    
    # Verify blame shows both human and AI before rebase
    echo "=== Blame BEFORE rebase ===" >&3
    blame_before=$(git-ai blame app.py)
    echo "$blame_before" >&3
    
    [[ "$blame_before" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in blame before rebase" >&3
        return 1
    }
    
    [[ "$blame_before" =~ "Test User" ]] || {
        echo "ERROR: 'Test User' not found in blame before rebase" >&3
        return 1
    }
    
    # Step 4: Switch to main and create 3 new commits (modifying app.py)
    # Main will add utility functions in the "Utility functions section"
    git checkout main
    
    # Commit 1 on main: Add logging import and first utility
    cat > app.py <<'EOF'
# Application Module
# This file contains the main application logic
import logging

def main():
    print("Application starting")

# Utility functions section
# Add utility functions below

def get_config():
    return {"debug": True}

# Data processing section  
# Add data processing functions below

# End of file
EOF
    git add app.py
    git commit -m "Main: Add logging and get_config utility"
    main_commit1=$(git rev-parse HEAD)
    echo "=== Main commit 1: $main_commit1 ===" >&3
    
    # Commit 2 on main: Add second utility function
    cat > app.py <<'EOF'
# Application Module
# This file contains the main application logic
import logging

def main():
    print("Application starting")

# Utility functions section
# Add utility functions below

def get_config():
    return {"debug": True}

def log_message(msg):
    logging.info(msg)

# Data processing section  
# Add data processing functions below

# End of file
EOF
    git add app.py
    git commit -m "Main: Add log_message utility"
    main_commit2=$(git rev-parse HEAD)
    echo "=== Main commit 2: $main_commit2 ===" >&3
    
    # Commit 3 on main: Add third utility function
    cat > app.py <<'EOF'
# Application Module
# This file contains the main application logic
import logging

def main():
    print("Application starting")

# Utility functions section
# Add utility functions below

def get_config():
    return {"debug": True}

def log_message(msg):
    logging.info(msg)

def handle_error(err):
    logging.error(f"Error: {err}")

# Data processing section  
# Add data processing functions below

# End of file
EOF
    git add app.py
    git commit -m "Main: Add handle_error utility"
    main_commit3=$(git rev-parse HEAD)
    echo "=== Main commit 3: $main_commit3 ===" >&3
    
    # Verify main has 3 commits ahead of common ancestor
    commits_ahead=$(git rev-list --count ${common_ancestor}..main)
    [[ "$commits_ahead" == "3" ]] || {
        echo "ERROR: Expected 3 commits ahead on main, got $commits_ahead" >&3
        return 1
    }
    
    echo "✓ Main branch has 3 commits ahead of common ancestor" >&3
    
    # Step 5: Rebase feature branch onto main
    echo "=== Rebasing feature branch onto main ===" >&3
    git checkout feature
    
    # Show state before rebase
    echo "=== Git log before rebase ===" >&3
    git log --oneline --graph --all >&3
    
    git rebase main
    
    # Show state after rebase
    echo "=== Git log after rebase ===" >&3
    git log --oneline --graph --all >&3
    
    # Step 6: Verify authorship is preserved after rebase
    feature_commit_after=$(git rev-parse HEAD)
    
    echo "=== Feature commit stats AFTER rebase ===" >&3
    stats_feature_after=$(get_stats_json "$feature_commit_after")
    echo "$stats_feature_after" >&3
    
    # Verify stats after rebase - they should match the original stats
    # The authorship attribution should be preserved during rebase
    echo "=== Comparing stats before and after rebase ===" >&3
    compare_json "$expected_feature_json" "$stats_feature_after" "Feature commit stats after rebase do not match expected" || return 1
    
    # Verify blame output after rebase
    echo "=== Blame AFTER rebase ===" >&3
    blame_after=$(git-ai blame app.py)
    echo "$blame_after" >&3
    
    [[ "$blame_after" =~ "mock_ai" ]] || {
        echo "ERROR: 'mock_ai' not found in blame after rebase" >&3
        echo "Line-level AI authorship was lost during rebase" >&3
        return 1
    }
    
    [[ "$blame_after" =~ "Test User" ]] || {
        echo "ERROR: 'Test User' not found in blame after rebase" >&3
        return 1
    }
    
    echo "✓ Authorship stats preserved after rebase" >&3
    
    # Verify the rebased commit is now on top of main's commits
    merge_base=$(git merge-base main feature)
    [[ "$merge_base" == "$main_commit3" ]] || {
        echo "ERROR: Feature branch not properly rebased onto main" >&3
        echo "Expected merge base: $main_commit3" >&3
        echo "Actual merge base: $merge_base" >&3
        return 1
    }
    
    # Verify feature branch is 1 commit ahead of main
    commits_ahead_after=$(git rev-list --count main..feature)
    [[ "$commits_ahead_after" == "1" ]] || {
        echo "ERROR: Expected feature to be 1 commit ahead of main, got $commits_ahead_after" >&3
        return 1
    }
    
    # Verify file content is preserved (both feature and main changes in same file)
    echo "=== Final file listing ===" >&3
    ls -la >&3
    
    echo "=== Final app.py content ===" >&3
    cat app.py >&3
    
    # Verify feature branch changes (process_data function added by feature)
    grep -q "process_data" app.py || {
        echo "ERROR: process_data function not found after rebase" >&3
        return 1
    }
    
    grep -q "input_data.upper()" app.py || {
        echo "ERROR: AI-contributed code (input_data.upper) not found after rebase" >&3
        return 1
    }
    
    # Verify main branch changes are present (utility functions added by main)
    grep -q "import logging" app.py || {
        echo "ERROR: logging import from main not found after rebase" >&3
        return 1
    }
    
    grep -q "get_config" app.py || {
        echo "ERROR: get_config function from main not found after rebase" >&3
        return 1
    }
    
    grep -q "log_message" app.py || {
        echo "ERROR: log_message function from main not found after rebase" >&3
        return 1
    }
    
    grep -q "handle_error" app.py || {
        echo "ERROR: handle_error function from main not found after rebase" >&3
        return 1
    }
    
    echo "✓ Successfully rebased feature branch with mixed authorship onto diverged main" >&3
    echo "✓ AI and Human authorship preserved after rebase" >&3
    echo "✓ All changes from both branches present in same file (app.py)" >&3
}
