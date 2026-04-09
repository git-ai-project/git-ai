#!/bin/bash
set -euo pipefail

echo "=== Human Checkpoint Example ==="
echo '{
  "type": "human",
  "repo_working_dir": "'$(pwd)'"
}' | cargo run -- checkpoint agent-v1 --hook-input stdin

echo -e "\n=== AI Agent Checkpoint Example ==="
echo '{
  "type": "ai_agent",
  "repo_working_dir": "'$(pwd)'",
  "transcript": {
    "messages": [
      {
        "type": "user",
        "text": "Please add error handling to this function",
        "timestamp": "2024-01-15T10:30:00Z"
      },
      {
        "type": "assistant", 
        "text": "I will add proper error handling using Result types",
        "timestamp": "2024-01-15T10:30:15Z"
      }
    ]
  },
  "agent_name": "claude-3-sonnet",
  "model": "claude-3-sonnet-20240229",
  "conversation_id": "conv_12345"
}' | cargo run -- checkpoint agent-v1 --hook-input stdin