import Foundation

/// Version details stamped into Info.plist by the build. The Nix build sets
/// them from the workspace version and the flake's git revision; local Xcode
/// builds keep the project defaults.
enum BuildInfo {
    static var summary: String {
        let info = Bundle.main.infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String ?? "unknown"
        let build = info["CFBundleVersion"] as? String ?? "unknown"
        let revision = info["OgygiaGitRevision"] as? String ?? "unknown"
        return "Ogygia \(version) (\(build), \(revision))"
    }
}
