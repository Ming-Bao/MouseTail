// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Kiore",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "Kiore",
            path: "Sources/Kiore"
        )
    ],
    swiftLanguageModes: [.v5]
)
