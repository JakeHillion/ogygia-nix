import SwiftUI

/// Which fleet this Mac follows. The daemon stores the values in the
/// `dev.ogygia.macos` defaults domain, where nix-darwin or a configuration
/// profile can set them instead.
struct SettingsView: View {
    let nebula: NebulaModel

    @State private var repositoryURL = ""
    @State private var branch = ""
    @State private var device = ""
    @State private var loaded = false
    @State private var saving = false
    @State private var message: String?

    var body: some View {
        Form {
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
        .padding()
        .frame(width: 460)
        .onAppear {
            // A menu bar app is never frontmost on its own.
            NSApplication.shared.activate()
            load()
        }
        .onChange(of: nebula.status) { load() }
    }

    private var managed: Bool { nebula.status?.settingsManaged ?? false }

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
