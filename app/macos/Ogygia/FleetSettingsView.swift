import SwiftUI

/// Which fleet this Mac follows, and how fetching its configuration went.
/// The daemon stores the values in the `dev.ogygia.macos` defaults domain,
/// where nix-darwin or a configuration profile can set them instead.
struct FleetSettingsView: View {
    let nebula: NebulaModel

    @State private var repositoryURL = ""
    @State private var branch = ""
    @State private var device = ""
    @State private var loaded = false
    @State private var saving = false
    @State private var message: String?

    var body: some View {
        Form {
            Section {
                TextField("Repository URL", text: $repositoryURL, prompt: Text("https://git.example.com/fleet.git"))
                TextField("Branch", text: $branch, prompt: Text("The repository's default branch"))
                TextField("Device", text: $device, prompt: Text("Name in ogygiaConfigurations"))
                if managed {
                    Text("These settings are managed by a configuration profile.")
                        .foregroundStyle(.secondary)
                }
                if let message {
                    Text(message)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                }
                HStack {
                    Spacer()
                    Button("Save") { save() }
                        .keyboardShortcut(.defaultAction)
                        .disabled(saving || managed || nebula.status == nil)
                }
            }
            .disabled(managed)

            Section("Configuration") {
                LabeledContent("Last updated") {
                    Text(lastUpdate)
                }
                if let error = nebula.status?.updateError {
                    Text(error)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                }
                HStack {
                    if nebula.status?.updating == true {
                        ProgressView()
                            .controlSize(.small)
                        Text("Fetching configuration…")
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Refresh Now") { nebula.refresh() }
                        .disabled(nebula.status == nil || nebula.status?.updating == true)
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Fleet")
        .onAppear { load() }
        .onChange(of: nebula.status) { load() }
    }

    private var managed: Bool { nebula.status?.settingsManaged ?? false }

    private var lastUpdate: String {
        guard let status = nebula.status else { return "Unknown" }
        return status.lastUpdate?.formatted(date: .abbreviated, time: .shortened) ?? "Never"
    }

    /// Fill the fields from the daemon once it has answered.
    private func load() {
        guard !loaded, let status = nebula.status else { return }
        repositoryURL = status.repositoryURL ?? ""
        branch = status.branch ?? ""
        device = status.device ?? ""
        loaded = true
    }

    private func save() {
        saving = true
        message = nil
        Task {
            message = await nebula.save(repositoryURL: repositoryURL, branch: branch, device: device)
            saving = false
        }
    }
}
