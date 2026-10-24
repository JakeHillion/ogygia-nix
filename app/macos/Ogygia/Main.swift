import AppKit

/// Start the menu bar app, or, given `--snapshots <directory>`, render every
/// settings pane to PNGs there and exit. Rendering never registers the
/// daemon or starts Sparkle, so it is safe on any Mac; see
/// app/macos/TESTING.md.
@main
enum Main {
    static func main() {
        let arguments = CommandLine.arguments
        if let flag = arguments.firstIndex(of: "--snapshots"), flag + 1 < arguments.count {
            let directory = URL(fileURLWithPath: arguments[flag + 1], isDirectory: true)
            MainActor.assumeIsolated {
                Snapshots.run(into: directory, live: arguments.contains("--live"))
            }
        } else {
            OgygiaApp.main()
        }
    }
}
