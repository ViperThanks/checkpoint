use std::path::PathBuf;
use std::process::Command;

/// BinaryLocator — locates the agent-aspect (or legacy checkpoint) binary.
///
/// Search order:
///   1. App resource dir `Binaries/` (release mode)
///   2. AGENT_ASPECT_DEV_BIN_DIR environment variable (dev mode)
///   3. PATH fallback via `which`

const BINARY_NAME: &str = "checkpoint";

/// Returns the full path to the agent-aspect binary, or None if not found.
pub fn locate_binary(resource_dir: Option<&PathBuf>) -> Option<PathBuf> {
    // 1. App resource dir Binaries/
    if let Some(res_dir) = resource_dir {
        let candidate = res_dir.join("Binaries").join(BINARY_NAME);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }

    // 2. AGENT_ASPECT_DEV_BIN_DIR env var
    if let Ok(dev_dir) = std::env::var("AGENT_ASPECT_DEV_BIN_DIR") {
        let candidate = PathBuf::from(dev_dir).join(BINARY_NAME);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }

    // 3. PATH fallback
    if let Ok(output) = Command::new("which").arg(BINARY_NAME).output() {
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

fn is_executable(path: &PathBuf) -> bool {
    path.exists() && {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
}
