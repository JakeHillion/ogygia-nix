import Cocoa
import SwiftUI

@main
struct OgygiaApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        Settings {
            EmptyView()
        }
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    var statusItem: NSStatusItem?
    var menu: NSMenu?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Create the status item in the menu bar
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        if let button = statusItem?.button {
            button.title = "Ogygia"
        }

        // Create the menu
        menu = NSMenu()

        let helloItem = NSMenuItem(title: "Hello world!", action: nil, keyEquivalent: "")
        helloItem.isEnabled = false
        menu?.addItem(helloItem)

        menu?.addItem(NSMenuItem.separator())

        let quitItem = NSMenuItem(title: "Quit", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        menu?.addItem(quitItem)

        statusItem?.menu = menu
    }
}
