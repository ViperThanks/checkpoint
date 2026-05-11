use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths;

/// BinaryLocator — locates the agent-aspect binary.
///
/// Search order:
///   1. App resource dir `Binaries/` (release mode)
///   2. AGENT_ASPECT_DEV_BIN_DIR environment variable (dev mode)
///   3. Running bridge state sibling path (dogfood / dev mode)
///   4. Repository target directories discovered from the current working dir
///   5. PATH fallback via `which`

const BINARY_NAME: &str = "agent-aspect";
const BRIDGE_BINARY_NAME: &str = "agent-aspect-bridge";

/// Returns the full path to the agent-aspect binary, or None if not found.
pub fn locate_binary(resource_dir: Option<&PathBuf>) -> Option<PathBuf> {
    locate_named_binary(resource_dir, BINARY_NAME)
}

/// Returns the full path to the bridge binary, or None if not found.
pub fn locate_bridge_binary(resource_dir: Option<&PathBuf>) -> Option<PathBuf> {
    locate_named_binary(resource_dir, BRIDGE_BINARY_NAME)
}

fn locate_named_binary(resource_dir: Option<&PathBuf>, binary_name: &str) -> Option<PathBuf> {
    // 1. App resource dir Binaries/
    if let Some(res_dir) = resource_dir {
        let candidate = res_dir.join("Binaries").join(binary_name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }

    // 2. AGENT_ASPECT_DEV_BIN_DIR env var
    if let Ok(dev_dir) = std::env::var("AGENT_ASPECT_DEV_BIN_DIR") {
        let candidate = PathBuf::from(dev_dir).join(binary_name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }

    // 3. Existing bridge state points at the running bridge binary. In dev
    // dogfood builds the CLI binary lives next to it in target/{debug,release}.
    if let Some(candidate) = sibling_from_bridge_state(binary_name) {
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }

    // 4. Repository target directories discovered from likely launch roots.
    for root in candidate_roots() {
        for profile in ["debug", "release"] {
            let candidate = root.join("target").join(profile).join(binary_name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    // 5. PATH fallback. GUI apps often have a small PATH, so this is only the
    // final escape hatch rather than the primary dev discovery mechanism.
    if let Ok(output) = Command::new("/usr/bin/which").arg(binary_name).output() {
        let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path_str.is_empty() {
            let candidate = PathBuf::from(&path_str);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

fn sibling_from_bridge_state(binary_name: &str) -> Option<PathBuf> {
    let data = std::fs::read_to_string(paths::bridge_state_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    let bridge_exe = value.get("exe")?.as_str()?;
    let bridge_path = PathBuf::from(bridge_exe);

    if binary_name == BRIDGE_BINARY_NAME && is_executable(&bridge_path) {
        return Some(bridge_path);
    }

    bridge_path.parent().map(|dir| dir.join(binary_name))
}

fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        collect_ancestors(&cwd, &mut roots);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            collect_ancestors(parent, &mut roots);
        }
    }
    roots
}

fn collect_ancestors(path: &Path, roots: &mut Vec<PathBuf>) {
    for ancestor in path.ancestors() {
        let candidate = ancestor.to_path_buf();
        if !roots.contains(&candidate) {
            roots.push(candidate);
        }
    }
}

fn is_executable(path: &PathBuf) -> bool {
    path.exists() && {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
}
