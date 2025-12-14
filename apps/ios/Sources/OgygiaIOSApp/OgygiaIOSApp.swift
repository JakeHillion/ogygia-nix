import SwiftUI

@main
struct OgygiaIOSApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}

struct ContentView: View {
    var body: some View {
        VStack(spacing: 20) {
            Text("Ogygia")
                .font(.largeTitle)
                .fontWeight(.bold)

            Text("Hello world!")
                .font(.title2)
                .foregroundColor(.secondary)
        }
        .padding()
    }
}
