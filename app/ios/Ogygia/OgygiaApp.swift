import SwiftUI

@main
struct OgygiaApp: App {
    var body: some Scene {
        WindowGroup {
            VStack(spacing: 8) {
                Text(BuildInfo.summary)
                Text("Not for production use")
                    .foregroundStyle(.secondary)
            }
            .padding()
        }
    }
}
