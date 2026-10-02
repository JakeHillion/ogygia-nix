import Foundation
import Observation
import Sparkle

/// Mirrors `SPUUpdater.canCheckForUpdates` so the menu can disable its
/// "Check for Updates…" item while a check is already running.
@MainActor
@Observable
final class UpdaterModel {
    private(set) var canCheckForUpdates = false

    @ObservationIgnored private let updater: SPUUpdater
    @ObservationIgnored private var observation: NSKeyValueObservation?

    init(updater: SPUUpdater) {
        self.updater = updater
        // Sparkle only changes this property on the main thread.
        observation = updater.observe(\.canCheckForUpdates, options: [.initial, .new]) {
            [weak self] updater, _ in
            let canCheck = updater.canCheckForUpdates
            MainActor.assumeIsolated { self?.canCheckForUpdates = canCheck }
        }
    }

    func checkForUpdates() {
        updater.checkForUpdates()
    }
}
