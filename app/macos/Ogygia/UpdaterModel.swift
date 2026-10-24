import Foundation
import Observation
import Sparkle

/// Mirrors `SPUUpdater.canCheckForUpdates` so the menu can disable its
/// "Check for Updates…" item while a check is already running, and the
/// updater's settings for the General pane.
@MainActor
@Observable
final class UpdaterModel {
    private(set) var canCheckForUpdates = false
    private(set) var lastUpdateCheckDate: Date?

    var automaticallyChecksForUpdates: Bool {
        didSet { updater.automaticallyChecksForUpdates = automaticallyChecksForUpdates }
    }

    @ObservationIgnored private let updater: SPUUpdater
    @ObservationIgnored private var observation: NSKeyValueObservation?

    init(updater: SPUUpdater) {
        self.updater = updater
        automaticallyChecksForUpdates = updater.automaticallyChecksForUpdates
        // Sparkle only changes this property on the main thread. It turns
        // back on as each check ends, which is when the check date moves.
        observation = updater.observe(\.canCheckForUpdates, options: [.initial, .new]) {
            [weak self] updater, _ in
            MainActor.assumeIsolated {
                self?.canCheckForUpdates = updater.canCheckForUpdates
                self?.lastUpdateCheckDate = updater.lastUpdateCheckDate
            }
        }
    }

    func checkForUpdates() {
        updater.checkForUpdates()
    }
}
