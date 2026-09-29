// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "MouseTail",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "MouseTail",
            path: "Sources/MouseTail"
        )
    ],
    swiftLanguageModes: [.v5]
)
