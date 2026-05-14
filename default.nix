
let
  # Pin Rust 1.93.0 via rust-overlay
  rustToolchain' = { pkgs, ... }:
    pkgs.rust-bin.stable."1.93.0".default.override {
      extensions = [
        "rust-src"
        "rust-analyzer"
        "llvm-tools-preview"
      ];
    }
  ;

  # Create a custom rustPlatform using the pinned toolchain
  rustPlatform' = { pkgs, git-ai, ... }: with git-ai.utils;
    pkgs.makeRustPlatform {
      cargo = rustToolchain;
      rustc = rustToolchain;
    };

  # Build the git-ai binary using the pinned Rust toolchain
  git-ai-unwrapped' = {pkgs, git-ai, ...}: with git-ai.utils;
    rustPlatform.buildRustPackage {
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
  wrapped' = { pkgs, git, git-ai, ... }: with git-ai.packages;
    pkgs.writeShellScriptBin "git-ai" ''
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
      exec ${unwrapped}/bin/git-ai "$@"
    '';

  # Wrapper for git command that preserves argv[0] as "git"
  # This is critical: when symlinked as "git", the wrapper must set argv[0]
  # to "git" so the Rust binary routes to handle_git() instead of handle_git_ai()
  wrapper' = {pkgs, git, git-ai, ...}: with git-ai.packages;
    pkgs.writeShellScriptBin "git" ''
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
      exec -a git ${unwrapped}/bin/git-ai "$@"
    '';

  # Create git-og wrapper that bypasses git-ai and calls real git directly
  # This is needed because git interprets argv[0] as a subcommand
  git-og' = {pkgs, git, ...}: 
    pkgs.writeShellScriptBin "git-og" ''
      exec ${git}/bin/git "$@"
    '';

  # Package without git wrapper - for Home Manager / environments with existing git
  git-ai-minimal' = { pkgs, git, git-ai, ... }: with git-ai.utils; with git-ai.packages;
    pkgs.symlinkJoin {
      name = "git-ai-minimal-${unwrapped.version}";
      paths = [ wrapped unwrapped git-og ];

      # Create libexec symlink for Fork compatibility
      # Fork looks for libexec relative to the git binary location
      postBuild = ''
        ln -s ${git}/libexec $out/libexec
      '';

      meta = unwrapped.meta // {
        description = unwrapped.meta.description + " (without git wrapper)";
      };
    };

  # Create a complete package with git wrapper (for standalone use)
  # The git-wrapper script ensures argv[0] is "git" when invoked as git
  git-ai-package' = { pkgs, git, git-ai, ... }: with git-ai.utils; with git-ai.packages;
    pkgs.symlinkJoin {
      name = "git-ai-${unwrapped.version}";
      paths = [ wrapped wrapper unwrapped git-og ];

      # Create libexec symlink for Fork compatibility
      # Fork looks for libexec relative to the git binary location
      postBuild = ''
        ln -s ${git}/libexec $out/libexec
      '';

      meta = unwrapped.meta // {
        description = unwrapped.meta.description + " (with git wrapper)";
      };
    };
in
  {pkgs, lib }: 
    lib.makeScope pkgs.newScope (self:
      { 
        git-ai =
          {
            utils = {
              rustToolchain = self.callPackage rustToolchain' { };
              rustPlatform = self.callPackage rustPlatform' { };
              git-og = self.callPackage git-og' { };
              wrapper = self.callPackage wrapper' { };
              wrapped = self.callPackage wrapped' { };
            };
            packages = { 
              git-ai = self.callPackage git-ai-package' { };
              default = self.callPackage git-ai-package' { };
              minimal = self.callPackage git-ai-minimal' { };
              unwrapped = self.callPackage git-ai-unwrapped' { };
            };
          }
        ;
      }
  )
