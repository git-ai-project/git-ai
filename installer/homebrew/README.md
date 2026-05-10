# Homebrew Formula

A Homebrew formula template for git-ai. Supports macOS (arm64/x64) and Linux (arm64/x64).

## How it works

The formula downloads the pre-built binary for the user's platform and installs:
- `git-ai` — the main binary
- `git` — a shim (same binary, dispatched by argv[0])

Both are placed in the Homebrew `bin/` directory which is already in PATH.

## Usage

```bash
# Install from tap
brew install git-ai-project/tap/git-ai

# Or install directly from the generated formula
brew install --formula ./git-ai.rb
```

## Generating a release formula

The `update-formula.sh` script substitutes version/checksums into the template:

```bash
./update-formula.sh \
  --version 1.4.6 \
  --repo git-ai-project/git-ai \
  --checksums path/to/SHA256SUMS \
  --output build/git-ai.rb
```

## CI integration

The release workflow generates the formula after building all binaries and computing checksums.
The formula can then be pushed to a Homebrew tap repository.
