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

#### Local-Only Mode

When ZooKeeper is not configured, the status command shows only the local host:

```
Ogygia config not found; showing local data only.
Host      ⚡ current    🥾 booted      🔜 next boot
---------------------------------------------------
hostname  a1b2c3d4e5f6  a1b2c3d4e5f6  g7h8i9j0k1l2
```

#### Fleet Mode with ZooKeeper

When ZooKeeper is configured, the status command shows all hosts in your fleet:

```
ZooKeeper fleet state (/nixos/versions via /run/current-system/sw/share/ogygia/config.toml):
Host              ⚡ current    🥾 booted      🔜 next boot
------------------------------------------------------------
* web01.dc1 (local) a1b2c3d4e5f6  a1b2c3d4e5f6  g7h8i9j0k1l2
web02.dc1          b2c3d4e5f6a1  b2c3d4e5f6a1  unknown
db01.dc2           c3d4e5f6a1b2  c3d4e5f6a1b2  c3d4e5f6a1b2
```

The status command shows:
- **⚡ current**: The currently active system configuration (`/run/current-system`)
- **🥾 booted**: The system that was booted (`/run/booted-system`)
- **🔜 next boot**: The system that will be used on next boot (`/nix/var/nix/profiles/system`)
- **`*` marker**: Indicates the local host
- **unknown**: Indicates the revision file is missing or the state hasn't been published yet

### ZooKeeper Fleet Visibility

Ogygia can connect to ZooKeeper to provide fleet-wide visibility of system build revisions. This allows you to see the current, booted, and next boot revisions for all hosts in your infrastructure from any machine.

#### Configuration

To enable ZooKeeper integration, add the following to your NixOS configuration:

```nix
{
  ogygia = {
    enable = true;
    domain = "example.com";  # Optional: base domain suffix to trim from hostnames in display
    zookeeper = {
      enable = true;
      endpoints = [
        "zk1.internal:2181"
        "zk2.internal:2181"
        "zk3.internal:2181"
      ];
      namespace = "/nixos/versions";  # Optional: ZooKeeper path prefix (default shown)
      timeoutSeconds = 10;            # Optional: connection timeout (default: 10)
    };
  };
}
```

This generates a configuration file at `/run/current-system/sw/share/ogygia/config.toml`:

```toml
[ogygia]
domain = "example.com"

[ogygia.zookeeper]
endpoints = ["zk1.internal:2181", "zk2.internal:2181", "zk3.internal:2181"]
namespace = "/nixos/versions"
timeout_seconds = 10
```

#### Environment Variables

You can override the CLI behavior with environment variables:

- **`OGYGIA_CONFIG`**: Override the path to the configuration file
  ```bash
  OGYGIA_CONFIG=/path/to/config.toml ogygia status
  ```

- **`OGYGIA_HOSTNAME`**: Override hostname detection
  ```bash
  OGYGIA_HOSTNAME=web01.example.com ogygia status
  ```

#### ZooKeeper Data Structure

**Note:** This implementation is read-only. To populate ZooKeeper with host data, you need a separate publisher daemon (not included in this feature). The publisher would monitor system state changes and write revision data to the znodes described below.

Ogygia expects data in ZooKeeper under the configured namespace with the following structure:

```
/nixos/versions/          # namespace (configurable)
├── web01/
│   ├── current          # contains: a1b2c3d4e5f6
│   ├── booted           # contains: a1b2c3d4e5f6
│   └── nextboot         # contains: g7h8i9j0k1l2
├── web02/
│   ├── current
│   ├── booted
│   └── nextboot
└── db01/
    ├── current
    ├── booted
    └── nextboot
```

#### Troubleshooting

**Connection Failures**

If the CLI cannot connect to ZooKeeper, it will display an error and fall back to local-only mode:

```
ZooKeeper fleet state (/nixos/versions via /run/current-system/sw/share/ogygia/config.toml):
Failed to read ZooKeeper from /run/current-system/sw/share/ogygia/config.toml: failed to connect to ZooKeeper at zk1:2181,zk2:2181. Check that the endpoints are reachable and the ZooKeeper service is running. Connection timeout: 10s. Showing local data only.
Host           ⚡ current    🥾 booted      🔜 next boot
-------------------------------------------------------
* web01 (local) a1b2c3d4e5f6  a1b2c3d4e5f6  g7h8i9j0k1l2
```

**Common Issues:**
- **ZooKeeper not running**: Ensure the ZooKeeper service is running on the configured endpoints
- **Network connectivity**: Verify the host can reach the ZooKeeper endpoints (check firewall rules)
- **Namespace doesn't exist**: This is normal before the publisher daemon creates the znodes
- **Permission denied**: Check ZooKeeper ACLs if authentication is enabled

**"unknown" Revisions**

The status display shows "unknown" in these cases:
- The build revision file doesn't exist (system not built with Ogygia enabled)
- The ZooKeeper znode is missing (publisher hasn't written data yet)
- The system state path doesn't exist yet (e.g., before first reboot)

**Hostname Detection Issues**

Ogygia detects the hostname using multiple fallback strategies:
1. `$OGYGIA_HOSTNAME` environment variable (if set)
2. `hostname -f` command (fully qualified domain name)
3. `$HOSTNAME` environment variable
4. `hostname` command (short name)
5. `gethostname()` syscall

If hostname detection isn't working as expected, use the `OGYGIA_HOSTNAME` environment variable to override it

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
