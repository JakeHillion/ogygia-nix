// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "OgygiaIOS",
    platforms: [
        .iOS(.v16)
    ],
    products: [
        .executable(
            name: "OgygiaIOS",
            targets: ["OgygiaIOSApp"])
    ],
    targets: [
        .executableTarget(
            name: "OgygiaIOSApp",
            path: "Sources/OgygiaIOSApp")
    ]
)
