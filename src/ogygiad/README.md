# ogygiad

A daemon for managing Ogygia island infrastructure.

## Features

### ZooKeeper Version Upload

Monitors the NixOS system state and uploads version information to ZooKeeper. This tracks:

- **current**: The currently running system (`/run/current-system`)
- **booted**: The system that was booted (`/run/booted-system`)
- **nextboot**: The system that will be used on next boot (`/nix/var/nix/profiles/system`)

Version information is read from `/sw/share/ogygia/build-revision` within each system path and uploaded to ZooKeeper at `/ogygia/versions/v1/{hostname}/{state}`.

The daemon uses file watching (via the `notify` crate) to detect changes and automatically update ZooKeeper when the system state changes. It caches versions to avoid unnecessary ZooKeeper updates.

## Configuration

Configuration is provided via a TOML file:

```toml
[zookeeper]
addresses = "zk1.example.com:2181,zk2.example.com:2181"
enable_version_upload = true
hostname = "node1.island.example.com"
```

See `ogygiad.toml.example` for a complete example.

## NixOS Module

When using the NixOS module, ogygiad is configured through the Nix configuration:

```nix
{
  ogygia = {
    enable = true;

    ogygiad = {
      enable = true;

      zookeeper = {
        addresses = "zk1.example.com:2181,zk2.example.com:2181";
        enableVersionUpload = true;
        hostname = config.networking.fqdn; # default
      };
    };
  };
}
```

The module automatically:
- Generates the TOML configuration file
- Creates a systemd service
- Applies security hardening (DynamicUser, ProtectSystem, etc.)
- Sets up read-only access to system paths

## Architecture

ogygiad is structured to support multiple daemonized processes:

- **Tokio**: Async runtime for managing concurrent tasks
- **Notify**: File system watching for detecting system changes
- **ZooKeeper Client**: For uploading version information
- **Modular Design**: Easy to add new daemon tasks in the future

Each feature (like ZooKeeper version upload) can be independently enabled/disabled via configuration.
