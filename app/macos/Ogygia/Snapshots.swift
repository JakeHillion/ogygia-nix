import AppKit
import SwiftUI

/// Renders every settings pane to a PNG, so a person or an agent can look at
/// the window without opening it by hand. From fixtures, every state worth
/// seeing is drawn with no daemon at all; with `--live`, the panes show what
/// the running daemon reports, as the end-to-end test does.
@MainActor
enum Snapshots {
    static func run(into directory: URL, live: Bool) {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        Task {
            do {
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                for scenario in live ? [try await liveScenario()] : fixtureScenarios() {
                    for pane in scenario.panes {
                        for appearance in scenario.appearances {
                            let name = "\(scenario.name)-\(pane.rawValue)\(appearance == .darkAqua ? "-dark" : "").png"
                            let url = directory.appending(path: name)
                            try await render(scenario, pane: pane, appearance: appearance, to: url)
                            print(url.path)
                        }
                    }
                }
                exit(0)
            } catch {
                FileHandle.standardError.write(Data("snapshots failed: \(error)\n".utf8))
                exit(1)
            }
        }
        app.run()
    }

    struct Scenario {
        var name: String
        var nebula: NebulaModel
        var mesh: MeshSource
        var panes: [SettingsView.Pane]
        var appearances: [NSAppearance.Name] = [.aqua]
        /// How long the panes take to settle: a live mesh comes over ssh.
        var settle: Duration = .milliseconds(800)
    }

    private static func render(
        _ scenario: Scenario, pane: SettingsView.Pane, appearance: NSAppearance.Name, to url: URL
    ) async throws {
        let updater = UpdaterModel(fixtureLastCheck: Date(timeIntervalSince1970: 1_792_000_000))
        let view = SettingsView(nebula: scenario.nebula, updater: updater, pane: pane)
            .environment(\.meshSource, scenario.mesh)
        let window = NSWindow(
            contentRect: CGRect(x: 0, y: 0, width: 860, height: 600),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: appearance)
        let host = NSHostingView(rootView: view)
        window.contentView = host
        // Off every screen: drawn, but never in anyone's way.
        window.setFrameOrigin(CGPoint(x: -20000, y: -20000))
        window.orderFrontRegardless()
        defer { window.close() }

        try await Task.sleep(for: scenario.settle)
        host.layoutSubtreeIfNeeded()
        guard let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) else {
            throw SnapshotError("could not make a bitmap for \(url.lastPathComponent)")
        }
        // The selected sidebar row draws as a dark bar: cacheDisplay cannot
        // draw the selection material in a window that is not key.
        host.cacheDisplay(in: host.bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else {
            throw SnapshotError("could not encode \(url.lastPathComponent)")
        }
        try png.write(to: url)
    }

    /// The daemon on this machine, which something other than the app
    /// started: the app never registers it here.
    private static func liveScenario() async throws -> Scenario {
        let nebula = NebulaModel(register: false)
        let deadline = ContinuousClock.now + .seconds(30)
        while nebula.status == nil {
            guard ContinuousClock.now < deadline else {
                throw SnapshotError("the daemon did not answer: \(nebula.unreachable ?? "no reply")")
            }
            try await Task.sleep(for: .milliseconds(200))
        }
        return Scenario(
            name: "live", nebula: nebula, mesh: .daemon,
            panes: SettingsView.Pane.allCases, settle: .seconds(5))
    }

    private static func fixtureScenarios() -> [Scenario] {
        let all = SettingsView.Pane.allCases
        let mesh = MeshSource { try Fixtures.mesh() }
        func failing(_ message: String) -> MeshSource {
            MeshSource { throw DaemonClient.Failure.daemon(message) }
        }
        return [
            Scenario(
                name: "running", nebula: NebulaModel(fixture: Fixtures.status()), mesh: mesh,
                panes: all, appearances: [.aqua, .darkAqua]),
            Scenario(
                name: "local", nebula: NebulaModel(fixture: Fixtures.status(Fixtures.local)), mesh: mesh,
                panes: [.nebulaConfig]),
            Scenario(
                name: "unconfigured", nebula: NebulaModel(fixture: Fixtures.status(Fixtures.unconfigured)),
                mesh: failing("Nebula is not running"),
                panes: [.fleet, .nebulaOverview, .nebulaMesh, .nebulaConfig]),
            Scenario(
                name: "failing",
                nebula: NebulaModel(fixture: Fixtures.status(Fixtures.failing), registration: .requiresApproval),
                mesh: failing("Nebula is not running"), panes: [.fleet, .nebulaOverview]),
            Scenario(
                name: "managed", nebula: NebulaModel(fixture: Fixtures.status(Fixtures.managed)), mesh: mesh,
                panes: [.fleet, .nebulaConfig]),
            Scenario(
                name: "unreachable",
                nebula: NebulaModel(
                    fixture: nil, unreachable: "Can't reach the network daemon: No such file or directory"),
                mesh: failing("Can't reach the network daemon: No such file or directory"),
                panes: [.fleet, .nebulaOverview, .nebulaConfig]),
            Scenario(
                name: "not-admin", nebula: NebulaModel(fixture: Fixtures.status()),
                mesh: failing("only an administrator can see the mesh, as it shows where every peer is"),
                panes: [.nebulaMesh, .nebulaPeers]),
        ]
    }
}

struct SnapshotError: Error, CustomStringConvertible {
    var description: String
    init(_ description: String) { self.description = description }
}

/// Daemon replies as the daemon writes them, so rendering them also checks
/// the app decodes the protocol.
private enum Fixtures {
    /// The base status with `changes`, a JSON object, merged over it.
    static func status(_ changes: String = "{}") -> DaemonProtocol.Status {
        func object(_ json: String) -> [String: Any] {
            // The fixtures are literals, so a bad one is a bug here.
            // swiftlint:disable:next force_try force_cast
            try! JSONSerialization.jsonObject(with: Data(json.utf8)) as! [String: Any]
        }
        let merged = object("{\(base)}").merging(object(changes)) { $1 }
        // swiftlint:disable:next force_try
        let line = try! JSONSerialization.data(withJSONObject: ["status": merged])
        // swiftlint:disable:next force_try
        var status = try! DaemonProtocol.decode(line).status!
        // The same build as the app, so no fixture says the daemon is stale.
        status.build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String
        return status
    }

    static func mesh() throws -> DaemonProtocol.Mesh {
        guard let mesh = try DaemonProtocol.decode(Data(meshLine.utf8)).mesh else {
            throw SnapshotError("the mesh fixture has no mesh")
        }
        return mesh
    }

    private static let base = """
        "build": "1", "repository_url": "https://git.example.com/fleet.git", "branch": null,
        "device": "studio", "settings_managed": false,
        "public_key": "-----BEGIN NEBULA X25519 PUBLIC KEY-----\\nTWzHB6c1lcFi0n9ZtKfV2q2N5i8GQmQqmWJ0c3xX0WY=\\n-----END NEBULA X25519 PUBLIC KEY-----\\n",
        "address": "10.42.0.20", "cert_expires": "2027-01-15T11:02:29Z", "updating": false,
        "last_update": "2026-10-24T08:40:00Z", "update_error": null,
        "nebula": {"state": "running", "since": "2026-10-24T07:00:00Z"},
        "config_mode": "fleet", "config_mode_managed": false,
        "fleet_config": \(fleetConfig),
        "local_config": {"lighthouses": [], "use_relays": false, "relays": [], "listen_port": 0, "inbound": [], "outbound": []}
        """

    private static let fleetConfig = """
        {"lighthouses": [{"address": "10.42.0.1", "endpoints": ["lighthouse.example.com:4242"]}],
         "use_relays": true, "relays": ["10.42.0.2"], "listen_port": 0,
         "inbound": [{"port": "any", "proto": "icmp", "host": "any"}, {"port": "22", "proto": "tcp", "groups": ["ssh"]}],
         "outbound": [{"port": "any", "proto": "any", "host": "any"}]}
        """

    static let local = """
        {"config_mode": "local",
        "local_config": {"lighthouses": [{"address": "10.42.0.1", "endpoints": ["lighthouse.example.com:4242", "192.0.2.10:4242"]}],
         "use_relays": true, "relays": ["10.42.0.2"], "listen_port": 4242,
         "inbound": [{"port": "any", "proto": "icmp", "host": "any"}, {"port": "5900", "proto": "tcp", "host": "laptop"}],
         "outbound": [{"port": "any", "proto": "any", "host": "any"}]}}
        """

    static let unconfigured = """
        {"repository_url": null, "device": null, "address": null, "cert_expires": null,
        "last_update": null, "nebula": {"state": "stopped"}, "fleet_config": null}
        """

    static let failing = """
        {"nebula": {"state": "exited", "reason": "level=ERROR msg=\\"Failed to get a tun/tap device\\" error=\\"operation not permitted\\""},
        "update_error": "error: studio has no certificate for its current spec (4f1c9a0e6b2d7c35a8e1f0b9d2c4e6a7).\\n       Run `ogygia nebula rekey --host studio` and commit the result."}
        """

    static let managed = """
        {"settings_managed": true, "config_mode_managed": true}
        """

    private static let meshLine = """
        {"mesh": {"hosts": [
          {"name": "laptop", "address": "10.42.0.21", "state": "no_tunnel", "lighthouse": false, "relay": false, "remote": null, "via": [], "cert_expires": null},
          {"name": "lighthouse", "address": "10.42.0.1", "state": "connected", "lighthouse": true, "relay": false, "remote": "203.0.113.7:4242", "via": [], "cert_expires": "2027-01-15T11:02:29Z"},
          {"name": "nas", "address": "10.42.0.3", "state": "connected", "lighthouse": false, "relay": false, "remote": "192.168.1.20:4242", "via": [], "cert_expires": "2027-01-02T09:00:00Z"},
          {"name": "phone", "address": "10.42.0.22", "state": "no_tunnel", "lighthouse": false, "relay": false, "remote": null, "via": [], "cert_expires": null},
          {"name": "printer", "address": "10.42.0.5", "state": "handshaking", "lighthouse": false, "relay": false, "remote": null, "via": [], "cert_expires": null},
          {"name": "relay", "address": "10.42.0.2", "state": "connected", "lighthouse": false, "relay": true, "remote": "198.51.100.4:4242", "via": [], "cert_expires": "2027-01-15T11:02:29Z"},
          {"name": "tv", "address": "10.42.0.4", "state": "connected", "lighthouse": false, "relay": false, "remote": null, "via": ["10.42.0.2"], "cert_expires": "2026-12-01T00:00:00Z"}
        ], "forwarding": []}}
        """
}
