//! Project trust. A repo's .melon/config.toml can define things that execute (hooks, MCP servers, verification
//! commands) or loosen safety. Cloning a repo must not be enough to run them, so those keys are ignored until the
//! user trusts that exact config file.
use crate::util::paths::resolve;
use std::collections::BTreeMap;
use std::path::Path;

/// Keys that execute code or relax protections when set by a project ("providers" could point your API keys at the
/// repo owner's server).
pub const GATED_KEYS: &[&str] = &[
    "hooks", "hooks_enabled", "mcp_servers", "verify_commands", "verify_rules", "approval", "shell_environment", "allowed_dirs", "sandbox",
    "sandbox_write_extra", "allow_private_network", "forbidden_paths", "providers",
];

fn store(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("trusted_projects.json")
}

fn digest(config_path: &Path) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(std::fs::read(config_path).unwrap_or_default()))
}

fn load(data_dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(store(data_dir)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Trusted only while the file still has the content the user approved.
pub fn is_trusted(project_dir: &Path, config_path: &Path, data_dir: &Path) -> bool {
    load(data_dir).get(&resolve(project_dir).to_string_lossy().to_string()) == Some(&digest(config_path))
}

pub fn trust(project_dir: &Path, config_path: &Path, data_dir: &Path) {
    let _ = std::fs::create_dir_all(data_dir);
    let mut records = load(data_dir);
    records.insert(resolve(project_dir).to_string_lossy().to_string(), digest(config_path));
    let _ = std::fs::write(store(data_dir), serde_json::to_string_pretty(&records).unwrap_or_default());
}
