"""Project trust. A repo's .melon/config.toml can define things that execute
(hooks, MCP servers, verification commands) or loosen safety (approval mode,
shell environment). Cloning a repo must not be enough to run them, so those
keys are ignored until the user trusts that exact config file."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path

# keys that execute code or relax protections when set by a project ("providers" could point your API keys at the repo owner's server)
GATED_KEYS = {"hooks", "hooks_enabled", "mcp_servers", "verify_commands", "verify_rules", "approval", "shell_environment", "allowed_dirs", "sandbox", "sandbox_write_extra", "allow_private_network", "forbidden_paths", "providers"}


def _store(data_dir: Path) -> Path:
    return data_dir / "trusted_projects.json"


def _digest(config_path: Path) -> str:
    return hashlib.sha256(config_path.read_bytes()).hexdigest()


def _load(data_dir: Path) -> dict[str, str]:
    try:
        return json.loads(_store(data_dir).read_text())
    except (OSError, ValueError):
        return {}


def is_trusted(project_dir: Path, config_path: Path, data_dir: Path) -> bool:
    """Trusted only while the file still has the content the user approved."""
    return _load(data_dir).get(str(project_dir.resolve())) == _digest(config_path)


def trust(project_dir: Path, config_path: Path, data_dir: Path) -> None:
    data_dir.mkdir(parents=True, exist_ok=True)
    records = _load(data_dir)
    records[str(project_dir.resolve())] = _digest(config_path)
    _store(data_dir).write_text(json.dumps(records, indent=1))
