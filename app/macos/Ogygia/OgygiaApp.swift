import Sparkle
import SwiftUI

@main
struct OgygiaApp: App {
    private let updaterController: SPUStandardUpdaterController
    @State private var updater: UpdaterModel
    @State private var nebula = NebulaModel()

    init() {
        let controller = SPUStandardUpdaterController(
            startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil)
        updaterController = controller
        _updater = State(initialValue: UpdaterModel(updater: controller.updater))
    }

    var body: some Scene {
        MenuBarExtra {
            Text(nebula.headline)
            ForEach(nebula.details, id: \.self) { line in
                Text(line)
            }
            if nebula.needsApproval {
                Button("Open Login Items Settings…") { nebula.openLoginItems() }
            }
            Divider()
            Button("Copy Public Key") { nebula.copyPublicKey() }
                .disabled(nebula.status?.publicKey == nil)
            Button("Refresh Configuration") { nebula.refresh() }
                .disabled(nebula.status == nil)
            SettingsLink { Text("Settings…") }
                .keyboardShortcut(",")
            Divider()
            Text(BuildInfo.summary)
            Text("Not for production use")
            Button("Check for Updates…") { updater.checkForUpdates() }
                .disabled(!updater.canCheckForUpdates)
            Divider()
            Button("Quit Ogygia") { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
        } label: {
            // A template image, so the system tints it to match the menu bar.
            Image("MenuBarIcon")
                .accessibilityLabel("Ogygia")
        }

        Settings {
            SettingsView(nebula: nebula, updater: updater)
        }
        .windowResizability(.contentMinSize)
    }
}
