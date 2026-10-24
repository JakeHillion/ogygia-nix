import ServiceManagement
import SwiftUI

/// Whether the daemon and Nebula are running, and this Mac's identity on the
/// mesh.
struct NebulaOverviewView: View {
    let nebula: NebulaModel

    var body: some View {
        Form {
            Section("Status") {
                LabeledContent("Network daemon") {
                    Text(daemonState)
                }
                if nebula.needsApproval {
                    HStack {
                        Spacer()
                        Button("Open Login Items Settings…") { nebula.openLoginItems() }
                    }
                }
                if let note = staleDaemon {
                    Text(note)
                        .foregroundStyle(.secondary)
                }
                if let status = nebula.status {
                    LabeledContent("Nebula") {
                        Text(nebulaState(status.nebula))
                    }
                    if case .exited(let reason) = status.nebula {
                        Text(reason)
                            .foregroundStyle(.red)
                            .textSelection(.enabled)
                    }
                }
            }
            if let status = nebula.status {
                Section("This Mac") {
                    LabeledContent("Address") {
                        Text(status.address ?? "No certificate yet")
                            .textSelection(.enabled)
                    }
                    if let expires = status.certExpires {
                        LabeledContent("Certificate expires") {
                            Text(expires.formatted(date: .abbreviated, time: .omitted))
                        }
                    }
                    if let key = status.publicKey {
                        LabeledContent("Public key") {
                            Button("Copy") { nebula.copyPublicKey() }
                        }
                        Text(key)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Nebula")
    }

    private var daemonState: String {
        switch nebula.registration {
        case .requiresApproval:
            return "Waiting for approval in System Settings"
        case .notFound:
            return "Not in this build"
        case .notRegistered:
            return nebula.registrationError.map { "Not installed: \($0)" } ?? "Not installed"
        case .enabled:
            break
        @unknown default:
            break
        }
        if nebula.status != nil {
            return "Running"
        }
        return nebula.unreachable ?? "Starting…"
    }

    /// The daemon restarts itself within a minute of an update replacing it;
    /// until then it is still the old build.
    private var staleDaemon: String? {
        let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String
        guard let running = nebula.status?.build, running != build else { return nil }
        return "The network daemon is from build \(running) and will restart shortly."
    }

    private func nebulaState(_ state: DaemonProtocol.Nebula) -> String {
        switch state {
        case .running(let since):
            "Running since \(since.formatted(date: .abbreviated, time: .shortened))"
        case .exited:
            "Stopped, restarting"
        case .stopped:
            nebula.status?.updating == true ? "Waiting for configuration" : "Not running"
        }
    }
}
