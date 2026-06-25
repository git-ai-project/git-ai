# Reproduction harness for issue #1657 / #1287:
# "daemon startup blocked: lock held" is reported when the real cause is a
# permission failure on the daemon lock file (created by an elevated/other user).
#
# This builds git-ai from the source tree it is invoked against and runs a
# scenario where the lock file is owned by root while a normal user tries to
# start the daemon. See repro-daemon-lock.sh for the scenario and README.md
# for usage.
#
# Build context must be the repo root, e.g.:
#   docker build -f docs/investigations/issue-1657/Dockerfile -t gitai-1657 .
#   docker run --rm gitai-1657

FROM rust:1.93-slim-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates pkg-config libssl-dev sudo \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
# Copy the source tree (the .git dir is included so GIT_AI_VERSION resolves; if
# you have a large worktree, a .dockerignore can trim it -- only src/ + Cargo.* +
# build.rs are required to compile the binary).
COPY . /src

# Debug build is sufficient: the detached-daemon startup path is compiled in all
# non-test builds (#[cfg(not(any(test, feature = "test-support")))]).
RUN cargo build --bin git-ai

# Unprivileged user used to trigger the permission failure.
RUN useradd --create-home --shell /bin/bash dev

COPY docs/investigations/issue-1657/repro-daemon-lock.sh /usr/local/bin/repro-daemon-lock.sh
RUN chmod +x /usr/local/bin/repro-daemon-lock.sh

ENV GIT_AI_BIN=/src/target/debug/git-ai
ENTRYPOINT ["/usr/local/bin/repro-daemon-lock.sh"]
