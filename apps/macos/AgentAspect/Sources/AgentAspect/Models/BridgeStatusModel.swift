/// BridgeStatusModel.swift — Parsed bridge status from `agent-aspect bridge status`
///
/// Captures the running state, PID, listen address, LAN / launchd / keep-awake
/// flags, token path, and relay status. Used by DiagnosticsView and AppState.

import Foundation

struct BridgeStatusModel {
    let isRunning: Bool
    let pid: Int?
    let addr: String?
    let lanEnabled: Bool
    let launchdLoaded: Bool
    let keepAwake: Bool
    let tokenPath: String?
    let relayStatus: String?

    /// A human-readable summary for display in the diagnostics grid.
    var displaySummary: String {
        if isRunning {
            let pidStr = pid.map { String($0) } ?? "?"
            let addrStr = addr ?? "unknown"
            return "running (pid \(pidStr)) at \(addrStr)"
        }
        return "stopped"
    }
}

extension BridgeStatusModel {

    /// Parse the raw stdout of `agent-aspect bridge status` into a model.
    ///
    /// Expected keys (case-insensitive, colon-separated):
    ///   Status: running / stopped
    ///   PID: <int>
    ///   Address: <host:port>
    ///   LAN: enabled / disabled
    ///   Launchd: loaded / not loaded
    ///   Keep-awake: enabled / disabled
    ///   Token: <path>
    ///   Relay: <status string>
    static func parse(_ raw: String) -> BridgeStatusModel {
        let lines = raw.components(separatedBy: .newlines)
        var map: [String: String] = [:]
        for line in lines {
            guard let colonIdx = line.firstIndex(of: ":") else { continue }
            let key = line[line.startIndex..<colonIdx]
                .trimmingCharacters(in: .whitespaces).lowercased()
            let val = line[line.index(after: colonIdx)...]
                .trimmingCharacters(in: .whitespaces)
            map[key] = val
        }

        // CLI outputs "bridge: running" (key is "bridge", not "status")
        let running = (map["bridge"] ?? map["status"])?.lowercased().contains("running") ?? false

        let pid: Int? = {
            guard let s = map["pid"], let n = Int(s) else { return nil }
            return n
        }()

        return BridgeStatusModel(
            isRunning: running,
            pid: pid,
            addr: map["addr"] ?? map["address"],
            lanEnabled: map["lan"]?.lowercased().contains("enabled") ?? false,
            launchdLoaded: map["launchd"]?.lowercased().contains("loaded") ?? false,
            keepAwake: map["keep-awake"]?.lowercased().contains("enabled") ?? false,
            tokenPath: map["token"],
            relayStatus: map["relay"]
        )
    }
}
