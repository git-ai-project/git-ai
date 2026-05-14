# Installing git-ai with Nix

This project provides a Nix flake for easy installation on NixOS, nix-darwin, or any system using Home Manager or Nix profiles.

## Quick Start

Try without installing:
```bash
nix run github:acunniffe/git-ai -- --version
```

Install to user profile:
```bash
nix profile install github:acunniffe/git-ai
```

## What's Included

The package provides three commands:

| Command | Description |
|---------|-------------|
| `git` | Routes through git-ai (tracks AI authorship) |
| `git-ai` | Direct git-ai commands |
| `git-og` | Bypasses git-ai, calls real git |

## Flake Outputs

```
packages.${system}.default   # Complete package with git wrapper
packages.${system}.minimal   # Without git symlink (for manual integration)
packages.${system}.unwrapped # Just the binary
devShells.${system}.default  # Development environment
nixosModules.default         # NixOS module
homeManagerModules.default   # Home Manager module (hooks and config only)
overlays.default             # Nixpkgs overlay
```

## Installation Methods

### 1. Home Manager with programs.git (Recommended)

The cleanest approach is to set git-ai as your git package and use the module for hooks.

Add the input to your flake:
```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    home-manager.url = "github:nix-community/home-manager";
    git-ai.url = "github:acunniffe/git-ai";
  };
}
```

In your Home Manager configuration:
```nix
{ inputs, system, ... }:

{
  imports = [ inputs.git-ai.homeManagerModules.default ];

  # Use git-ai as the git implementation
  programs.git = {
    enable = true;
    package = inputs.git-ai.packages.${system}.default;
    # ... your other git settings (signing, aliases, etc.)
  };

  # Enable git-ai hooks for IDE/agent integration
  programs.git-ai = {
    enable = true;
    installHooks = true;  # Runs git-ai install-hooks on activation
  };
}
```

This approach:
- Replaces the standard git with git-ai throughout your environment
- Installs IDE/agent hooks automatically
- Creates `~/.git-ai/config.json` with the correct git path
- Avoids package conflicts

### 2. nix-darwin with Home Manager

```nix
{
  inputs = {
    darwin.url = "github:lnl7/nix-darwin";
    home-manager.url = "github:nix-community/home-manager";
    git-ai.url = "github:acunniffe/git-ai";
  };

  outputs = { darwin, home-manager, git-ai, nixpkgs, ... }: {
    darwinConfigurations.myhost = darwin.lib.darwinSystem {
      system = "aarch64-darwin";
      modules = [
        home-manager.darwinModules.home-manager
        {
          home-manager.users.myuser = { pkgs, ... }: {
            imports = [ git-ai.homeManagerModules.default ];

            programs.git = {
              enable = true;
              package = git-ai.packages.${pkgs.system}.default;
            };

            programs.git-ai = {
              enable = true;
              installHooks = true;
            };
          };
        }
      ];
    };
  };
}
```

### 3. NixOS System-Wide

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    git-ai.url = "github:acunniffe/git-ai";
  };

  outputs = { nixpkgs, git-ai, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        git-ai.nixosModules.default
        {
          programs.git-ai = {
            enable = true;
            installHooks = true;
          };

          # Add git-ai to system packages
          environment.systemPackages = [
            git-ai.packages.x86_64-linux.default
          ];
        }
      ];
    };
  };
}
```

### 4. Direct Package (Standalone)

If not using Home Manager's `programs.git`, add the package directly:
```nix
{ inputs, pkgs, ... }:

{
  home.packages = [
    inputs.git-ai.packages.${pkgs.system}.default
  ];
}
```

**Note:** This may conflict if you also have `programs.git.enable = true`. Use the `minimal` package to avoid conflicts:
```nix
home.packages = [
  inputs.git-ai.packages.${pkgs.system}.minimal  # No git symlink
];
```

### 5. Using the Overlay

```nix
{
  nixpkgs.overlays = [ inputs.git-ai.overlays.default ];

  # Then use:
  home.packages = [ pkgs.git-ai ];
}
```

## Development

Enter a development shell with Rust toolchain:
```bash
nix develop github:acunniffe/git-ai
```

Or clone and develop locally:
```bash
git clone https://github.com/acunniffe/git-ai
cd git-ai
nix develop

cargo build
cargo test
cargo run -- --version
```

## Local Flake Development

For developing from a local checkout:
```nix
{
  inputs.git-ai.url = "git+file:///path/to/git-ai";
}
```

## Module Options

### homeManagerModules.default

The Home Manager module handles hooks and configuration only (not package installation).

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | `false` | Enable git-ai hooks and config |
| `package` | package | flake default | The git-ai package (for hooks) |
| `installHooks` | bool | `true` | Run `git-ai install-hooks` on activation |

### nixosModules.default

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | `false` | Enable git-ai |
| `package` | package | flake default | The git-ai package to use |
| `installHooks` | bool | `true` | Run `git-ai install-hooks` on activation |
| `setGitAlias` | bool | `true` | Add git-ai to system PATH |

## Platforms

Supported systems:
- `x86_64-linux`
- `aarch64-linux`
- `x86_64-darwin`
- `aarch64-darwin`
