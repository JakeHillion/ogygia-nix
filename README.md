<p align="center">
  <img src="assets/logo.png" alt="Ogygia Logo" width="200"/>
</p>

<h1 align="center">Ogygia</h1>

<p align="center">
  <strong>NixOS Configuration Management and Version Tracking</strong>
</p>

<p align="center">
  <a href="https://deepwiki.com/JakeHillion/ogygia-nix"><img src="https://deepwiki.com/badge.svg" alt="Ask DeepWiki"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green.svg" alt="License: MIT"></a>
</p>

---

Ogygia is a NixOS configuration management tool that provides system version tracking and status reporting capabilities. It enables you to track configuration revisions across different system states (current, booted, and next boot) and provides a clean CLI interface for inspecting system state.

Named after Calypso's mythical island, Ogygia aims to help you build your own private island in the Nix ecosystem—a self-contained, resilient infrastructure that you control. Today, Ogygia provides the foundation with configuration revision tracking and system state inspection. Tomorrow, it will grow into a comprehensive fleet management platform with:

- **🌐 Nebula Mesh Network**: Secure overlay networking with simple, opinionated management tools
- **🔄 Intelligent Updates**: Automatic updates with complete tracking and rollback capabilities
- **🎯 Impact Analysis**: Identify which hosts are affected by configuration changes before deployment
- **📊 Fleet Visibility**: Real-time monitoring of what's running across your entire infrastructure
- **🏗️ Distributed Builds**: Seamlessly leverage your fleet's compute power for faster Nix builds
- **⚡ Distributed Systems**: First-class support for building resilient clusters (ZooKeeper, etcd, and beyond)

Whether you're managing a handful of servers or orchestrating a complex distributed system, Ogygia is designed to grow with your infrastructure while maintaining the simplicity and reproducibility that makes Nix powerful.

### Philosophy

**Ogygia is extremely opinionated.** Rather than providing endless configuration knobs, it makes strong decisions about how to achieve its goals with minimal input from you. The result is a fleet of NixOS machines that remain fully yours—you can use all the normal NixOS tools and techniques you know and love. But the Ogygia coordination layer itself "just works," handling the complexity of distributed fleet management so you can focus on what makes your infrastructure unique.

### History

Ogygia distills the best patterns and tools from a battle-tested home lab NixOS setup. All of the features that arrive in Ogygia—revision tracking, distributed builds, resilient clusters—have been running in production for some time, proving their worth in real-world use. However, in their original form, these capabilities were deeply interleaved with specific infrastructure requirements, making them impossible for others to adopt.

Ogygia extracts these battle-tested patterns into standalone, reusable components. What was once a tangled web of bespoke configuration becomes a coherent toolkit that anyone can use. You get the benefit of years of iteration and refinement, without needing to understand or replicate the complex context in which these features were born.

## Features

- **📝 Configuration Revision Tracking**: Automatically embed Git revision information into your NixOS system closure
- **🔍 System Status Inspection**: CLI tool to view build revisions across different system states
- **⚙️ NixOS Module Integration**: Easy integration into existing NixOS configurations via flake
- **📦 Cachix Support**: Pre-built binaries available via Cachix for faster installations
- **🦀 Built with Rust**: Fast, reliable CLI written in Rust using Clap

## Installation

### As a NixOS Module

Add Ogygia to your flake inputs:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    ogygia.url = "github:JakeHillion/ogygia-nix";
  };

  outputs = { self, nixpkgs, ogygia, ... }: {
    nixosConfigurations.yourhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ogygia.nixosModules.default
        {
          ogygia.enable = true;
          ogygia.domain = "island.example.com";
        }
      ];
    };
  };
}
```

### Using the CLI Tool

Run the CLI directly without installation:

```bash
nix run github:JakeHillion/ogygia-nix
```

Or add it to your system packages or home-manager configuration.

## Usage

### Configuration Revision Tracking

To enable configuration revision tracking in your NixOS configuration, set `system.configurationRevision` in your flake:

```nix
system.configurationRevision = nixpkgs.lib.mkIf (self ? rev) self.rev;
```

This ensures that:
- When the flake is clean (committed), the git revision is embedded in the system closure
- When the flake is dirty (uncommitted changes), the revision is set to "unknown"

The revision will be written to `/run/current-system/sw/share/ogygia/build-revision` when `ogygia.versions.build_revision.enable` is enabled (which happens automatically when `ogygia.enable` is set).

### Status Command

View build revisions for different system states:

```bash
ogygia status
```

Example output:
```
⚡ Current system      a1b2c3d4e5f6
🥾 Booted system       a1b2c3d4e5f6
🔜 Next boot system    g7h8i9j0k1l2
```

The status command shows:
- **⚡ Current system**: The currently active system configuration (`/run/current-system`)
- **🥾 Booted system**: The system that was booted (`/run/booted-system`)
- **🔜 Next boot system**: The system that will be used on next boot (`/nix/var/nix/profiles/system`)

## Cachix

Pre-built binaries are available via Cachix:

```nix
nixConfig = {
  extra-substituters = [
    "https://ogygia.cachix.org"
  ];
  extra-trusted-public-keys = [
    "ogygia.cachix.org-1:xb4bnMPeWgSP81Xs0Vl7ZU4Ez7Ul65qp/EoZ40pDaWo="
  ];
};
```

## License

MIT License - Copyright (c) 2025 Jake Hillion

See [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please feel free to submit issues and pull requests.

---

<p align="center">
  <sub>Built with ❤️ using Nix and Rust</sub>
</p>
