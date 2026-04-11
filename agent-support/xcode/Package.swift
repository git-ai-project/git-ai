// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "git-ai-xcode-watcher",
    platforms: [.macOS(.v12)],
    targets: [
        .executableTarget(
            name: "git-ai-xcode-watcher",
            path: "Sources/git-ai-xcode-watcher"
        )
    ]
)
