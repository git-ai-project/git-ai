import Foundation
import CoreServices

// MARK: - Globals
var watchedPaths: [String] = []
var pendingFiles: [String: [String: String]] = [:]  // repoRoot -> [path: content]
var debounceItems: [String: DispatchWorkItem] = []  // repoRoot -> work item
var repoRootCache: [String: String?] = [:]          // dir -> repoRoot (nil = not a repo)
let queue = DispatchQueue(label: "io.gitai.xcode-watcher", qos: .utility)
let gitAiBin: String = {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    let dev = "\(home)/.git-ai/bin/git-ai"
    if FileManager.default.fileExists(atPath: dev) { return dev }
    return "git-ai"
}()

// MARK: - Utilities
func findRepoRoot(for filePath: String) -> String? {
    let dir = (filePath as NSString).deletingLastPathComponent
    if let cached = repoRootCache[dir] {
        return cached
    }
    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: "/usr/bin/git")
    proc.arguments = ["-C", dir, "rev-parse", "--show-toplevel"]
    let pipe = Pipe()
    proc.standardOutput = pipe
    proc.standardError = Pipe()
    try? proc.run()
    proc.waitUntilExit()
    let result: String?
    if proc.terminationStatus == 0 {
        let out = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        result = out?.isEmpty == false ? out : nil
    } else {
        result = nil
    }
    repoRootCache[dir] = result
    return result
}

func readFileContents(_ path: String) -> String? {
    try? String(contentsOfFile: path, encoding: .utf8)
}

func shouldSkip(_ path: String) -> Bool {
    let skip = ["/.git/", "/DerivedData/", "/xcuserdata/", "/.build/", ".DS_Store"]
    return skip.contains { path.contains($0) }
}

func fireCheckpoint(repoRoot: String) {
    guard let files = pendingFiles[repoRoot], !files.isEmpty else { return }
    pendingFiles[repoRoot] = nil

    let payload: [String: Any] = [
        "editor": "xcode",
        "editor_version": "unknown",
        "extension_version": "1.0.0",
        "cwd": repoRoot,
        "edited_filepaths": Array(files.keys),
        "dirty_files": files
    ]

    guard let jsonData = try? JSONSerialization.data(withJSONObject: payload),
          let jsonStr = String(data: jsonData, encoding: .utf8) else { return }

    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: gitAiBin)
    proc.arguments = ["checkpoint", "known_human", "--hook-input", "stdin"]
    proc.currentDirectoryURL = URL(fileURLWithPath: repoRoot)
    let stdinPipe = Pipe()
    proc.standardInput = stdinPipe
    proc.standardOutput = Pipe()
    proc.standardError = Pipe()
    try? proc.run()
    stdinPipe.fileHandleForWriting.write(jsonStr.data(using: .utf8)!)
    stdinPipe.fileHandleForWriting.closeFile()
    proc.waitUntilExit()
}

// Must be called on `queue`.
func scheduleDebounce(repoRoot: String) {
    debounceItems[repoRoot]?.cancel()
    let item = DispatchWorkItem { fireCheckpoint(repoRoot: repoRoot) }
    debounceItems[repoRoot] = item
    queue.asyncAfter(deadline: .now() + 0.5, execute: item)
}

// MARK: - FSEvents callback
let callback: FSEventStreamCallback = { (_, _, numEvents, eventPaths, _, _) in
    guard let paths = Unmanaged<CFArray>.fromOpaque(eventPaths).takeUnretainedValue() as? [String] else { return }
    // Collect candidate file paths on the RunLoop thread (cheap checks only).
    var candidates: [String] = []
    for path in paths {
        guard !shouldSkip(path) else { continue }
        var isDir: ObjCBool = false
        guard FileManager.default.fileExists(atPath: path, isDirectory: &isDir),
              !isDir.boolValue else { continue }
        candidates.append(path)
    }
    guard !candidates.isEmpty else { return }
    // Dispatch all heavy work (git subprocess, file reads, debounce scheduling) onto `queue`
    // so that `pendingFiles`, `debounceItems`, and `repoRootCache` are only accessed from
    // a single serial queue, eliminating data races.
    queue.async {
        var roots = Set<String>()
        for path in candidates {
            guard let root = findRepoRoot(for: path) else { continue }
            guard let content = readFileContents(path) else { continue }
            if pendingFiles[root] == nil { pendingFiles[root] = [:] }
            pendingFiles[root]![path] = content
            roots.insert(root)
        }
        for root in roots { scheduleDebounce(repoRoot: root) }
    }
}

// MARK: - Main
var args = CommandLine.arguments.dropFirst()
var paths: [String] = []
var i = args.startIndex
while i < args.endIndex {
    if args[i] == "--path", args.index(after: i) < args.endIndex {
        i = args.index(after: i)
        paths.append(args[i])
    }
    i = args.index(after: i)
}
if paths.isEmpty { paths = [FileManager.default.currentDirectoryPath] }

// Canonicalize paths
let watchPaths = paths.map { ($0 as NSString).standardizingPath } as CFArray

var context = FSEventStreamContext(version: 0, info: nil, retain: nil, release: nil, copyDescription: nil)
guard let stream = FSEventStreamCreate(
    kCFAllocatorDefault,
    callback,
    &context,
    watchPaths,
    FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
    0.1,
    FSEventStreamCreateFlags(kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer)
) else {
    fputs("git-ai-xcode-watcher: failed to create FSEvents stream\n", stderr)
    exit(1)
}

FSEventStreamScheduleWithRunLoop(stream, CFRunLoopGetCurrent(), CFRunLoopMode.defaultMode.rawValue)
FSEventStreamStart(stream)
print("git-ai-xcode-watcher: watching \(paths.joined(separator: ", "))")
CFRunLoopRun()
