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
   task build
   ```

4. **Run the tests**:
   ```bash
   task test
   ```

### Using a development build locally

It's often helpful to point your `git-ai` to a development build. The dev script builds the binary and installs it to `~/.git-ai/bin/git-ai`, replacing the production binary so you can test changes with real git repositories.

```bash
task dev
```

If `~/.git-ai` isn't set up yet, the script will run the installer automatically first.

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

The project uses standard Rust formatting. Please run `task fmt` and `task lint` before committing your changes.


## Getting Help

If you have questions about contributing, feel free to open an issue or reach out to the maintainers.

- **Discord**: https://discord.gg/XJStYvkb5U
- **Office Hours**: https://calendly.com/d/cxjh-z79-ktm/meeting-with-git-ai-authors
