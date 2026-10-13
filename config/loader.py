from pathlib import Path
from typing import Any

from dotenv import load_dotenv
try:
    from platformdirs import user_config_dir, user_data_dir
except ModuleNotFoundError:
    def user_config_dir(appname: str) -> str:
        return str(Path.home() / ".config" / appname)

    def user_data_dir(appname: str) -> str:
        return str(Path.home() / ".local" / "share" / appname)
try:
    import tomllib as tomli
except ModuleNotFoundError:
    import tomli

from config.config import Config
from config.trust import GATED_KEYS, is_trusted, trust
from utils.errors import ConfigError
import logging

logger = logging.getLogger(__name__)
CONFIG_FILE_NAME = "config.toml"

AGENT_MD_FILE = "AGENT.MD"


def get_config_dir() -> Path:
    return Path(user_config_dir("ai-agent"))


def get_data_dir() -> Path:
    return Path(user_data_dir("ai-agent"))


def get_system_config_path() -> Path:
    return get_config_dir() / CONFIG_FILE_NAME


def _parse_toml(path: Path):
    try:
        with open(path, "rb") as f:
            return tomli.load(f)
    except tomli.TOMLDecodeError as e:
        raise ConfigError(f"Invalid TOML in {path}: {e}", config_file=str(path)) from e
    except (OSError, IOError) as e:
        raise ConfigError(
            f"Failed to read config file {path}: {e}", config_file=str(path)
        ) from e


def _get_project_config(cwd: Path) -> Path | None:
    current = cwd.resolve()
    agent_dir = current / ".ai-agent"

    if agent_dir.is_dir():
        config_file = agent_dir / CONFIG_FILE_NAME
        if config_file.is_file():
            return config_file

    return None


INSTRUCTION_FILES = ("AGENTS.md", "CLAUDE.md", AGENT_MD_FILE)
INLINE_LIMIT = 6000
PROFILES_DIR = Path(__file__).resolve().parent.parent / "profiles"


def _outline(text: str, max_lines: int = 45) -> str:
    rows = [
        f"  line {i}: {line.strip()}"
        for i, line in enumerate(text.splitlines(), 1)
        if line.startswith("#") and not line.startswith("####")
    ]
    return "\n".join(rows[:max_lines])


def _instructions_for(cwd: Path) -> str | None:
    """AGENTS.md / CLAUDE.md from cwd up to the repo root, closest first.

    Short files go into the prompt as-is. A 50KB rulebook would eat the context
    on every turn, so big ones contribute their headings and the model reads
    only the sections it needs (read_file with offset/limit)."""
    blocks: list[str] = []
    seen: set[Path] = set()
    for d in [cwd.resolve(), *cwd.resolve().parents][:8]:
        for name in INSTRUCTION_FILES:
            f = d / name
            if not f.is_file() or f in seen:
                continue
            text = f.read_text(encoding="utf-8", errors="replace")
            if len(text.strip()) < 40:  # a stub like "@AGENTS.md"
                continue
            seen.add(f)
            if len(text) <= INLINE_LIMIT:
                blocks.append(f"## {f}\n{text}")
            else:
                blocks.append(
                    f"## {f} ({len(text)} chars, too long to inline)\n"
                    "Read the sections relevant to your task with read_file "
                    "(offset/limit) BEFORE editing. Section map:\n" + _outline(text)
                )
            break  # one instruction file per directory
        if (d / ".git").exists():
            break
    return "\n\n".join(blocks) or None


def _load_profile(name: str | None, cwd: Path) -> dict[str, Any] | None:
    """Repository profile: extra rules/instructions for a known project.
    Picked by name, or by a `match` entry equal to any component of cwd."""
    if not PROFILES_DIR.is_dir():
        return None
    parts = {p.lower() for p in cwd.resolve().parts}
    for f in sorted(PROFILES_DIR.glob("*.toml")):
        data = _parse_toml(f)
        if (name and f.stem == name) or (
            not name and parts & {m.lower() for m in data.get("match", [])}
        ):
            data["profile"] = f.stem
            return data
    return None


def _merge_dicts(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    result = base.copy()
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = _merge_dicts(result[key], value)
        else:
            result[key] = value

    return result


def load_config(cwd: Path | None, trust_project: bool = False) -> Config:
    cwd = cwd or Path.cwd()
    # Only the harness's own .env (where API_KEY lives). The target repo's .env
    # is never opened: it holds that project's secrets, and loading it would
    # also hand them to every shell command the agent runs.
    load_dotenv(Path(__file__).resolve().parent.parent / ".env")

    system_path = get_system_config_path()

    config_dict: dict[str, Any] = {}

    if system_path.is_file():
        try:
            config_dict = _parse_toml(system_path)
        except ConfigError:
            logger.warning(f"Skipping invalid system config: {system_path}")

    project_path = _get_project_config(cwd)
    ignored: list[str] = []
    if project_path:
        try:
            project_config_dict = _parse_toml(project_path)
            gated = sorted(k for k in project_config_dict if k in GATED_KEYS)
            if gated and not (trust_project or is_trusted(cwd, project_path, get_data_dir())):
                # executable / safety-relaxing settings need the user's say-so first
                ignored = gated
                project_config_dict = {k: v for k, v in project_config_dict.items() if k not in GATED_KEYS}
            elif gated and trust_project:
                trust(cwd, project_path, get_data_dir())
            config_dict = _merge_dicts(config_dict, project_config_dict)
        except ConfigError:
            logger.warning(f"Skipping invalid project config: {project_path}")

    if "cwd" not in config_dict:
        config_dict["cwd"] = cwd

    profile = _load_profile(config_dict.get("profile"), cwd)
    profile_text = None
    if profile:
        profile_text = profile.pop("instructions", None)
        for k in ("match", "name", "version"):
            profile.pop(k, None)
        config_dict = _merge_dicts(profile, config_dict)  # project config wins

    if "developer_instructions" not in config_dict:
        parts = [profile_text, _instructions_for(cwd)]
        joined = "\n\n".join(p for p in parts if p)
        if joined:
            config_dict["developer_instructions"] = joined

    try:
        config = Config(**config_dict)
    except Exception as e:
        raise ConfigError(f"Invalid configuration: {e}") from e
    config.ignored_project_keys = ignored

    return config
