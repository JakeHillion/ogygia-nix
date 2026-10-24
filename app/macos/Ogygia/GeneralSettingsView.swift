import ServiceManagement
import SwiftUI

/// The app itself: starting at login, updates and the build.
struct GeneralSettingsView: View {
    let updater: UpdaterModel

    @State private var launchAtLogin = SMAppService.mainApp.status == .enabled
    @State private var loginItemError: String?

    var body: some View {
        Form {
            Section {
                Toggle("Open at login", isOn: Binding(
                    get: { launchAtLogin },
                    set: { setLaunchAtLogin($0) }))
                if let loginItemError {
                    Text(loginItemError)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                }
            }
            Section("Updates") {
                Toggle("Check for updates automatically", isOn: Binding(
                    get: { updater.automaticallyChecksForUpdates },
                    set: { updater.automaticallyChecksForUpdates = $0 }))
                LabeledContent("Last checked") {
                    Text(updater.lastUpdateCheckDate?.formatted(date: .abbreviated, time: .shortened) ?? "Never")
                }
                HStack {
                    Spacer()
                    Button("Check for Updates…") { updater.checkForUpdates() }
                        .disabled(!updater.canCheckForUpdates)
                }
            }
            Section("About") {
                LabeledContent("Version") {
                    Text(BuildInfo.summary)
                        .textSelection(.enabled)
                }
                Text("Ogygia for macOS is not for production use.")
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .navigationTitle("General")
    }

    private func setLaunchAtLogin(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            loginItemError = nil
        } catch {
            loginItemError = error.localizedDescription
        }
        // The user may have to allow it in System Settings first.
        launchAtLogin = SMAppService.mainApp.status == .enabled
    }
}
