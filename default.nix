{ pkgs, git, ... }:

let
  # Pin Rust 1.93.0 via rust-overlay
  rustToolchain = pkgs.rust-bin.stable."1.93.0".default.override {
    extensions = [
      "rust-src"
      "rust-analyzer"
      "llvm-tools-preview"
    ];
  };

  # Create a custom rustPlatform using the pinned toolchain
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };

  # Build the git-ai binary using the pinned Rust toolchain
  git-ai-unwrapped = rustPlatform.buildRustPackage {
    pname = "git-ai";
    version = "1.4.9";

    src = ./.;

    cargoLock = {
      lockFile = ./Cargo.lock;
    };

    # Prevent openssl-sys from vendoring OpenSSL (which requires perl).
    # Instead, link against the system OpenSSL provided by buildInputs.
    OPENSSL_NO_VENDOR = "1";

    # Native build inputs needed for rusqlite with bundled SQLite
    nativeBuildInputs = with pkgs; [
      pkg-config
    ] ++ [
      rustPlatform.bindgenHook  # For rusqlite bundled builds
    ];

    # Build inputs for runtime dependencies
    buildInputs = with pkgs; [
      # rusqlite bundled mode compiles its own SQLite, but needs these headers
      sqlite
      # openssl-sys needs system OpenSSL headers and libraries
      openssl
    ] ++ lib.optionals stdenv.hostPlatform.isDarwin [
      # macOS-specific dependencies
      libiconv
      apple-sdk_15
    ];

    # Tests require git and specific setup
    doCheck = false;

    meta = with pkgs.lib; {
      description = "AI-powered Git wrapper that tracks AI-generated code changes";
      homepage = "https://github.com/acunniffe/git-ai";
      license = licenses.gpl3Plus;
      maintainers = [ ];
      mainProgram = "git-ai";
      platforms = platforms.unix;
    };
  };

  # Wrapped version that sets up the git-ai environment properly
  git-ai-wrapped = pkgs.writeShellScriptBin "git-ai" ''
    # Ensure config directory exists
    mkdir -p "$HOME/.git-ai"

    # Create config.json if it doesn't exist
    if [ ! -f "$HOME/.git-ai/config.json" ]; then
      # Find the system git (not our wrapper)
      GIT_PATH="${git}/bin/git"
      cat > "$HOME/.git-ai/config.json" <<EOF
    {
      "git_path": "$GIT_PATH"
    }
    EOF
    fi

    # Execute git-ai with all arguments
    exec ${git-ai-unwrapped}/bin/git-ai "$@"
  '';

  # Wrapper for git command that preserves argv[0] as "git"
  # This is critical: when symlinked as "git", the wrapper must set argv[0]
  # to "git" so the Rust binary routes to handle_git() instead of handle_git_ai()
  git-wrapper = pkgs.writeShellScriptBin "git" ''
    # Ensure config directory exists
    mkdir -p "$HOME/.git-ai"

    # Create config.json if it doesn't exist
    if [ ! -f "$HOME/.git-ai/config.json" ]; then
      # Find the system git (not our wrapper)
      GIT_PATH="${git}/bin/git"
      cat > "$HOME/.git-ai/config.json" <<EOF
    {
      "git_path": "$GIT_PATH"
    }
    EOF
    fi

    # Execute git-ai with argv[0] set to "git" to trigger passthrough mode
    # The -a flag ensures argv[0] is "git" regardless of the actual binary path
    exec -a git ${git-ai-unwrapped}/bin/git-ai "$@"
  '';

  # Create git-og wrapper that bypasses git-ai and calls real git directly
  # This is needed because git interprets argv[0] as a subcommand
  git-og = pkgs.writeShellScriptBin "git-og" ''
    exec ${git}/bin/git "$@"
  '';

  # Package without git wrapper - for Home Manager / environments with existing git
  git-ai-minimal = pkgs.symlinkJoin {
    name = "git-ai-minimal-${git-ai-unwrapped.version}";
    paths = [ git-ai-wrapped git-ai-unwrapped git-og ];

    # Create libexec symlink for Fork compatibility
    # Fork looks for libexec relative to the git binary location
    postBuild = ''
      ln -s ${git}/libexec $out/libexec
    '';

    meta = git-ai-unwrapped.meta // {
      description = git-ai-unwrapped.meta.description + " (without git wrapper)";
    };
  };

  # Create a complete package with git wrapper (for standalone use)
  # The git-wrapper script ensures argv[0] is "git" when invoked as git
  git-ai-package = pkgs.symlinkJoin {
    name = "git-ai-${git-ai-unwrapped.version}";
    paths = [ git-ai-wrapped git-wrapper git-ai-unwrapped git-og ];

    # Create libexec symlink for Fork compatibility
    # Fork looks for libexec relative to the git binary location
    postBuild = ''
      ln -s ${git}/libexec $out/libexec
    '';

    meta = git-ai-unwrapped.meta // {
      description = git-ai-unwrapped.meta.description + " (with git wrapper)";
    };
  };
in
  {
    utils = {
      inherit rustToolchain;
      inherit rustPlatform;
    };
    packages = { 
      git-ai = git-ai-package;
      default = git-ai-package;
      minimal = git-ai-minimal;
      unwrapped = git-ai-unwrapped;
    };
  }
