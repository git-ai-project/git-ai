# Contributing to Git AI

Thank you for your interest in contributing to `git-ai`. This is a cool moment for the industry and we're all here to build ~~a~~ the standard for tracking AI code. 

## Getting Started

### Prerequisites

- Rust https://rustup.rs/ (compiler and tooling)
- Taskfile https://taskfile.dev/ (modern make)

### Development Setup

1. **Fork the repository** on GitHub

2. **Clone your fork**:
   ```bash
   git clone https://github.com/YOUR_USERNAME/git-ai.git
   cd git-ai
   ```

3. **Build the project**:
   ```bash
   cargo build
   ```

4. **Run the tests**:
   ```bash
   cargo test
   ```

5. **Check code coverage** (optional but recommended):
   ```bash
   task coverage
   ```

   The project maintains a minimum code coverage threshold of **50%** (enforced in CI). This threshold is based on the current coverage rounded down to the nearest 5%. Pull requests that reduce coverage below this threshold will fail CI checks.

### (Option 1) Putting a development build on your path

It's often helpful to point your `git` and `git-ai`  to a development build of `git-ai`. 

First add the development symlinks and build a debug build:

```bash
sh scripts/dev-symlinks.sh
task debug:local 
```

This will create symlinks to your development build, allowing you to test your changes with real git repositories.

### (Option 2) Running with Cargo

You can run specific `git-ai` commands directly with cargo. Because you're not addressing `git` or `git-ai` you need to tell `cargo` which codepath it should hit:

```bash
GIT_AI=git cargo run -- status
GIT_AI=git-ai cargo run -- checkpoint
```

## Contributing Changes

### Before You Start

- **Check existing issues**: Look for related issues or feature requests
- **For new features or architectural changes**: We encourage you to chat with the core maintainers first to discuss your approach. This helps ensure your contribution aligns with the project's direction and saves you time.

### Submitting a Pull Request

1. Create a new branch for your changes:
   ```bash
   git checkout -b my-feature-branch
   ```

2. Make your changes and commit them with clear, descriptive messages

3. Push to your fork:
   ```bash
   git push origin my-feature-branch
   ```

4. Open a Pull Request against the main repository

5. **Reference any related issues** in your PR description (e.g., "Fixes #123" or "Related to #456")

6. Wait for review from the maintainers

## Code Style

The project uses standard Rust formatting. Please run `cargo fmt` before committing your changes.


## Getting Help

If you have questions about contributing, feel free to open an issue or reach out to the maintainers.

- **Discord**: https://discord.gg/XJStYvkb5U
- **Office Hours**: https://calendly.com/d/cxjh-z79-ktm/meeting-with-git-ai-authors
