# Testing the macOS app

The app and its daemon are tested in four layers. Every layer runs without a
person, and nothing in them can disturb the Mac it runs on or take that Mac
off its mesh: the layers that run on a real Mac need no root, and the one that
needs root refuses to run anywhere but a disposable virtual machine.

| Layer | Command | Runs on | Covers |
|---|---|---|---|
| 1. Unit | `cargo test -p ogygia-macos-nebulad` (also in `nix flake check`) | anywhere | config, mesh, render, protocol, settings |
| 2. Daemon | `nix run .#test-macos-daemon` | any Mac | the daemon end to end, unprivileged |
| 3. Settings window | `nix run .#test-macos-ui` | any Mac with a login session | every pane rendered to PNG |
| 4. System | `nix run .#test-macos-system -- <Ogygia.app>` | a VM only: CI, or your own | the installed app as root |

Run the commands from the root of a checkout.

## 2. The daemon, unprivileged

`src/ogygia-macos-nebulad/tests/daemon.rs` runs the real daemon in-process with
an `Env` pointing everything at a temporary directory: its state, its socket,
its preferences (a JSON file rather than the `dev.ogygia.macos` domain) and
its sshd port. Nebula runs with no tun device. Around it the test generates a
CA and a fleet repository, which the daemon evaluates with the real `nix`
through a `file://` URL, and starts a lighthouse and a peer, all on loopback.
It checks the status, `set_settings`, `set_config` in both modes, the debug
sshd and the `mesh` request against real Nebula output.

It never touches `/Library`, `/var/run/ogygia`, the defaults domain, ports 4242
or 4243, or the Mac's own daemon, so it is safe beside a running Ogygia.

## 3. The settings window

The app has a snapshot mode: `Ogygia --snapshots <dir>` renders every pane in
every state worth seeing (running, managing locally, unconfigured, failing,
managed by a profile, daemon unreachable, not an administrator, and the
running state in dark mode) into PNGs and exits. It reads fixtures, never
registers the daemon and never starts Sparkle. `nix run .#test-macos-ui`
builds the app with the system's Xcode and renders into
`target/macos-snapshots`.

**Look at the PNGs for every pane your change touches.** They are the only
way to see the window without opening it. The fixtures are daemon replies in
`app/macos/Ogygia/Snapshots.swift`; add a scenario there when a change adds a
state.

The selected sidebar row draws as a dark bar with no label: the capture cannot
draw the selection material in a window that is not key. That is the capture,
not the app.

## 4. The system test

`src/ogygia-macos-nebulad/tests/system.rs` installs a built `Ogygia.app` under
`/Library/Ogygia-test` and has launchd run its daemon as root, which brings up
a real utun. It then:

- joins a throwaway mesh of a lighthouse, a relay and a NAS
- pings across the mesh
- moves to a local config, blocks the direct path to the NAS with pf, and
  checks the NAS is reached through the relay
- checks a non-administrator is refused the mesh
- renders the settings window from the live daemon (`--snapshots --live`)
- replaces the daemon's executable as an update does, and checks launchd
  starts the new one and the mesh comes back

It runs only where `sysctl kern.hv_vmm_present` is 1 and no Ogygia is
installed, and refuses otherwise. It cleans up after itself, but is meant for
machines that are thrown away.

It skips one thing: the app registers its daemon with SMAppService, which
needs a person to approve it in System Settings, so the test hands launchd the
same job directly.

### Running it

CI's `macos-app` job runs all of layers 2 to 4 on GitHub's macOS runners,
which are disposable VMs, and uploads the logs and every PNG as the
`macos-tests` artifact. To run them on a change before it has a pull request,
push it to a branch under `test/`:

```sh
jj bookmark set test/<change-id> -r <change>
jj git push -b test/<change-id> --allow-new
gh run watch "$(gh run list --branch test/<change-id> --limit 1 --json databaseId -q '.[0].databaseId')"
gh run download <run-id> -n macos-tests -D target/macos-tests
jj bookmark delete test/<change-id> && jj git push -b test/<change-id>
```

Pushing a `test/` branch publishes the change, so do it only when you have
been allowed to push.

## What stays manual

- Approving the daemon in System Settings' Login Items, which SMAppService
  requires of a person.
- Sparkle downloading and installing a real update from the published appcast.
- Notarisation and Gatekeeper's first-launch checks.
