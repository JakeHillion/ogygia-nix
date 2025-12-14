// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Ogygia",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(
            name: "Ogygia",
            targets: ["OgygiaApp"])
    ],
    targets: [
        .executableTarget(
            name: "OgygiaApp",
            path: "Sources/OgygiaApp")
    ]
)
