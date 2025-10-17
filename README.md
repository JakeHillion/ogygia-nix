# ogygia-nix

## How to use

### Configuration revision tracking

To enable configuration revision tracking in your NixOS configuration, you need to set `system.configurationRevision` in your flake. Add this to your NixOS configuration:

```nix
system.configurationRevision = nixpkgs.lib.mkIf (self ? rev) self.rev;
```

This ensures that:
- When the flake is clean (committed), the git revision is embedded in the system closure
- When the flake is dirty (uncommitted changes), the revision is set to "unknown"

The revision will be written to `/run/current-system/sw/share/ogygia/build-revision` when `ogygia.versions.build_revision.enable` is enabled (which happens automatically when `ogygia.enable` is set).
