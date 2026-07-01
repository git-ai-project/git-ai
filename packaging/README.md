# Git AI packaging

This directory contains package-manager installer scaffolding for Git AI.

Package outputs must install `git-ai` only. They must not install a `git`
wrapper, `git.exe` shim, `git-og`, or any other executable that changes Git
command routing. Per-user trace2 and editor/agent setup remains the
responsibility of `git-ai install-hooks` or `git-ai setup-package`.

The release workflow builds signed/notarized production packages when the
required Apple and Azure signing secrets are configured. Dry-run releases can
build unsigned packages for validation.
