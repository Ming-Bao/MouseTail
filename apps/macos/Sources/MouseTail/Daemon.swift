import Foundation

enum DaemonError: LocalizedError {
    case notRunning
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .notRunning: return "MouseTail isn't running."
        case .failed(let message): return message
        }
    }
}

/// Talks to the MouseTail daemon over its local socket: one JSON request per line, one JSON
/// reply per line.
struct DaemonClient {
    static let socketPath: String = {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return support.appendingPathComponent("MouseTail/mousetail.sock").path
    }()

    var path = DaemonClient.socketPath

    func call(_ request: [String: Any], timeout: TimeInterval = 5) async throws -> Data {
        try await Task.detached(priority: .userInitiated) {
            try self.callBlocking(request, timeout: timeout)
        }.value
    }

    func decode<T: Decodable>(_ type: T.Type, _ request: [String: Any]) async throws -> T {
        let data = try await call(request)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: data)
    }

    private func callBlocking(_ request: [String: Any], timeout: TimeInterval) throws -> Data {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw DaemonError.notRunning }
        defer { close(fd) }

        var tv = timeval(tv_sec: Int(timeout), tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8CString)
        guard bytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else { throw DaemonError.notRunning }
        withUnsafeMutableBytes(of: &addr.sun_path) { dst in
            bytes.withUnsafeBytes { dst.copyMemory(from: $0) }
        }
        let connected = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else { throw DaemonError.notRunning }

        var line = try JSONSerialization.data(withJSONObject: request)
        line.append(0x0A)
        let written = line.withUnsafeBytes { write(fd, $0.baseAddress, line.count) }
        guard written == line.count else { throw DaemonError.notRunning }

        var reply = Data()
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while reply.last != 0x0A {
            let n = read(fd, &buffer, buffer.count)
            if n <= 0 { break }
            reply.append(buffer, count: n)
        }
        guard let object = try JSONSerialization.jsonObject(with: reply) as? [String: Any] else {
            throw DaemonError.failed("Unexpected reply from MouseTail.")
        }
        if object["ok"] as? Bool != true {
            throw DaemonError.failed(object["error"] as? String ?? "Something went wrong.")
        }
        return reply
    }
}

/// Runs the bundled daemon (Contents/MacOS/mousetaild) for as long as the app is open.
/// If a daemon is already running (e.g. during development) the app just uses that one.
@MainActor
final class DaemonProcess {
    private var process: Process?

    var bundledDaemon: URL? {
        let url = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/mousetaild")
        return FileManager.default.isExecutableFile(atPath: url.path) ? url : nil
    }

    func ensureRunning() async {
        if (try? await DaemonClient().call(["cmd": "status"], timeout: 1)) != nil { return }
        if let process, process.isRunning { return }
        guard let daemon = bundledDaemon else { return }

        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Logs/MouseTail")
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        let logURL = logs.appendingPathComponent("mousetail.log")
        if !FileManager.default.fileExists(atPath: logURL.path) {
            FileManager.default.createFile(atPath: logURL.path, contents: nil)
        }

        let p = Process()
        p.executableURL = daemon
        p.arguments = ["run"]
        if let log = try? FileHandle(forWritingTo: logURL) {
            log.seekToEndOfFile()
            p.standardOutput = log
            p.standardError = log
        }
        do {
            try p.run()
            process = p
        } catch {
            NSLog("MouseTail: couldn't start daemon: \(error)")
        }
    }

    func stop() {
        guard let process, process.isRunning else { return }
        process.terminate()
        process.waitUntilExit()
    }
}
