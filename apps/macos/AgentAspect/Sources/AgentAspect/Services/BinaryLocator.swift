/// BinaryLocator.swift — Locates the agent-aspect binary
///
/// Search order:
///   1. App bundle Resources/Binaries/ (release mode)
///   2. AGENT_ASPECT_DEV_BIN_DIR environment variable (dev mode)
///   3. PATH fallback via /usr/bin/which

import Foundation

final class BinaryLocator {

    /// The name of the CLI binary to search for.
    /// The existing Rust project produces `agent-aspect` as the CLI binary.
    private let binaryName = "agent-aspect"

    // MARK: - Binary discovery

    /// Returns the full path to the agent-aspect binary, or nil if not found.
    func locateBinary() -> URL? {
        // 1. App bundle Resources/Binaries/
        if let bundlePath = Bundle.main.resourcePath {
            let candidate = URL(fileURLWithPath: bundlePath)
                .appendingPathComponent("Binaries")
                .appendingPathComponent(binaryName)
            if FileManager.default.isExecutableFile(atPath: candidate.path) {
                return candidate
            }
        }

        // 2. AGENT_ASPECT_DEV_BIN_DIR env var
        if let devDir = ProcessInfo.processInfo.environment["AGENT_ASPECT_DEV_BIN_DIR"] {
            let candidate = URL(fileURLWithPath: devDir).appendingPathComponent(binaryName)
            if FileManager.default.isExecutableFile(atPath: candidate.path) {
                return candidate
            }
        }

        // 3. PATH fallback
        if let pathResult = runCommand("/usr/bin/which", arguments: [binaryName]) {
            let trimmed = pathResult.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty && FileManager.default.isExecutableFile(atPath: trimmed) {
                return URL(fileURLWithPath: trimmed)
            }
        }

        return nil
    }

    // MARK: - Process execution

    /// Run a command and return its stdout. Returns nil on failure.
    private func runCommand(_ path: String, arguments: [String]) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = arguments

        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe() // discard stderr

        do {
            try process.run()
        } catch {
            return nil
        }

        process.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        return String(data: data, encoding: .utf8)
    }
}
