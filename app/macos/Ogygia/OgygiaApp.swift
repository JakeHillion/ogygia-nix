import Sparkle
import SwiftUI

@main
struct OgygiaApp: App {
    private let updaterController: SPUStandardUpdaterController
    @State private var updater: UpdaterModel

    init() {
        let controller = SPUStandardUpdaterController(
            startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil)
        updaterController = controller
        _updater = State(initialValue: UpdaterModel(updater: controller.updater))
    }

    var body: some Scene {
        MenuBarExtra("Ogygia", systemImage: "network") {
            Text(BuildInfo.summary)
            Divider()
            Button("Check for Updates…") { updater.checkForUpdates() }
                .disabled(!updater.canCheckForUpdates)
            Divider()
            Button("Quit Ogygia") { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
        }
    }
}
