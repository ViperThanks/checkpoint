/// AgentAspectPaths.swift — Centralized path resolution for Agent Aspect data
///
/// Resolves files under the single canonical `~/.agent-aspect/` directory.

import Foundation

enum AgentAspectPaths {

    // MARK: - Base directories

    /// Primary data directory.
    static func dataDir() -> String {
        expandTilde("~/.agent-aspect")
    }

    // MARK: - Bridge files

    static func bridgePortPath() -> String {
        resolveFile("bridge.port")
    }

    static func bridgeStatePath() -> String {
        resolveFile("bridge.state.json")
    }

    // TODO(M41.4): wire bridgeTokenPath to settings / token display
    static func bridgeTokenPath() -> String {
        resolveFile("bridge.token")
    }

    // TODO(M41.2): wire bridgePasswordPath to auto-fill login
    static func bridgePasswordPath() -> String {
        resolveFile("bridge.password")
    }

    // MARK: - Logs

    static func daemonLogPath() -> String {
        resolveFile("agent-aspectd.log")
    }

    // TODO(M41.4): wire bridgeStdoutLogPath to log viewer
    static func bridgeStdoutLogPath() -> String {
        resolveFile("agent-aspect-bridge.stdout.log")
    }

    // TODO(M41.4): wire bridgeStderrLogPath to log viewer
    static func bridgeStderrLogPath() -> String {
        resolveFile("agent-aspect-bridge.stderr.log")
    }

    // MARK: - Database

    static func auditDBPath() -> String {
        resolveFile("audit.db")
    }

    // MARK: - Helpers

    /// Resolve a file name inside the data directory.
    private static func resolveFile(_ name: String) -> String {
        (dataDir() as NSString).appendingPathComponent(name)
    }

    private static func expandTilde(_ path: String) -> String {
        (path as NSString).expandingTildeInPath
    }
}
