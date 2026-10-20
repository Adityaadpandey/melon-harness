import re
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
    return Path(user_config_dir("melon"))


def get_data_dir() -> Path:
    return Path(user_data_dir("melon"))


def move_old_dirs() -> None:
    """The app used to keep its sessions, traces and lessons under "ai-agent". Move them into "melon" so --resume and
    everything learned keep working after the rename. When "melon" already exists (an old copy of the app was still
    running, or something wrote there first) the two are merged: whatever melon lacks is moved in, and a file both
    have stays melon's. Example: ai-agent/sessions/a.json joins melon/sessions/ next to the b.json already there."""
    for base in (user_data_dir, user_config_dir):  # the same folder on macOS, two on Linux
        old, new = Path(base("ai-agent")), Path(base("melon"))
        if old.is_dir() and old != new:
            try:
                _merge_into(old, new)
            except OSError as e:
                logger.warning("could not move %s to %s: %s", old, new, e)


def _merge_into(old: Path, new: Path) -> None:
    if not new.exists():
        old.rename(new)
        return
    for item in old.iterdir():
        target = new / item.name
        if not target.exists():
            item.rename(target)
        elif item.is_dir() and target.is_dir():
            _merge_into(item, target)
    try:
        old.rmdir()  # only goes once everything has moved; a file both had keeps the old folder around
    except OSError:
        pass


def save_last_model(name: str) -> None:
    """Remember the model the user picked so the next launch starts on it."""
    try:
        d = get_data_dir()
        d.mkdir(parents=True, exist_ok=True)
        (d / "last_model").write_text(name.strip())
    except OSError:
        pass  # losing this only means falling back to the configured default


def load_last_model() -> str | None:
    try:
        return (get_data_dir() / "last_model").read_text().strip() or None
    except OSError:
        return None


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
    agent_dir = current / ".melon"

    if agent_dir.is_dir():
        config_file = agent_dir / CONFIG_FILE_NAME
        if config_file.is_file():
            return config_file

    return None


INSTRUCTION_FILES = ("AGENTS.md", "CLAUDE.md", AGENT_MD_FILE)
# Claude Code loads a whole CLAUDE.md, even tunyt's 48k-char AGENTS.md (~18k tokens). It sits in the cached
# prefix, so after the first call it costs almost nothing, and an outline alone hid every rule from the model.
# ponytail: one fixed cap; past it a rulebook still falls back to the outline
INLINE_LIMIT = 90_000
PROFILES_DIR = Path(__file__).resolve().parent.parent / "profiles"


def _outline(text: str, max_lines: int = 100) -> str:
    rows = [
        f"  line {i}: {line.strip()}"
        for i, line in enumerate(text.splitlines(), 1)
        if line.startswith("#") and not line.startswith("####")
    ]
    return "\n".join(rows[:max_lines])


def _with_imports(f: Path, text: str) -> str:
    """Inline `@other.md` lines (Claude Code style), one level deep, files next to f only."""
    def swap(m):
        target = f.parent / m.group(1)
        try:
            return target.read_text(encoding="utf-8", errors="replace") if target.is_file() and target != f else m.group(0)
        except OSError:
            return m.group(0)

    return re.sub(r"^@(\S+\.md)\s*$", swap, text, flags=re.M | re.I)


def _house_rules(text: str, limit: int = 1500) -> str:
    """Lines of a long rulebook about commits, comments, specs and plans, which must never be skipped."""
    hits = [ln.strip() for ln in text.splitlines() if re.search(r"commit|comment|\bspec\b|\bplan\b|ask before", ln, re.I)]
    return "\n".join(hits)[:limit]


TASTE_HEAD = re.compile(r"^# Taste \(Continuously Learned.*?\n|^\[cmd\]:.*\n|^See \[[^\]]*\](\([^)]*\))?\s*\n", re.M)


def taste_files(d: Path) -> list[Path]:
    """Command Code's learned preferences in a folder: .commandcode/taste/taste.md plus one per topic
    (design/taste.md, ui/taste.md, ...). The top file only links to the topic ones, so all are read."""
    t = d / ".commandcode" / "taste"
    return [f for f in [t / "taste.md", *sorted(t.glob("*/taste.md"))] if f.is_file()]


def taste_text(f: Path) -> str:
    """A taste file minus the boilerplate header and the "See [design/taste.md]" pointers."""
    try:
        return TASTE_HEAD.sub("", f.read_text(encoding="utf-8", errors="replace")).strip()
    except OSError:
        return ""


def _taste_for(dirs: list[Path], home: Path) -> str | None:
    """Taste from the repo folders, then the user's global ones (Command Code's and melon's own).
    It is learned, not written down, so it ranks below AGENTS.md / CLAUDE.md and the user's request."""
    blocks = []
    for d in [*dirs, home, home / ".melon"]:
        for f in taste_files(d):
            if text := taste_text(f):
                blocks.append(f"### {f}\n{text}")
    if not blocks:
        return None
    return ("## Learned taste (remembered preferences, each with a confidence; follow them by default, but the "
            "user's request and the instruction files above win when they disagree)\n\n" + "\n\n".join(blocks))


def _instructions_for(cwd: Path, home: Path | None = None) -> str | None:
    """AGENTS.md / CLAUDE.md from cwd up to the repo root, closest first, then the
    user's own ~/.claude/CLAUDE.md (their commit style, comment style, preferences).

    Every file found is used, not just one per directory, so a CLAUDE.md next to an
    AGENTS.md is not silently ignored.

    Short files go into the prompt as-is. A 50KB rulebook would eat the context
    on every turn, so big ones contribute their headings and the model reads
    only the sections it needs (read_file with offset/limit)."""
    blocks: list[str] = []
    seen: set[Path] = set()
    texts: set[str] = set()
    dirs: list[Path] = []
    for d in [cwd.resolve(), *cwd.resolve().parents][:8]:
        dirs.append(d)
        if (d / ".git").exists():
            break
    repo_dirs = dirs[::-1]  # outermost first, so a subfolder's taste comes after the repo's
    home = home or Path.home()
    dirs.append(home / ".claude")
    for d in dirs:
        for name in INSTRUCTION_FILES:
            f = d / name
            if not f.is_file() or f.resolve() in seen:
                continue
            text = _with_imports(f, f.read_text(encoding="utf-8", errors="replace"))
            if len(text.strip()) < 40 or text.strip() in texts:  # a stub like "@AGENTS.md", or a copy
                continue
            seen.add(f.resolve())
            texts.add(text.strip())
            if len(text) <= INLINE_LIMIT:
                blocks.append(f"## {f}\n{text}")
            else:
                blocks.append(
                    f"## {f} ({len(text)} chars, too long to inline)\n"
                    "Read the sections relevant to your task with read_file "
                    "(offset/limit) BEFORE editing. Section map:\n" + _outline(text)
                    + "\nRules that always apply (commits, comments, specs, plans):\n" + _house_rules(text)
                )
    if taste := _taste_for(repo_dirs, home):
        blocks.append(taste)
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


def load_config(cwd: Path | None, trust_project: bool = False, pi_mode: bool | None = None) -> Config:
    cwd = cwd or Path.cwd()
    # Only the harness's own .env (where API_KEY lives). The target repo's .env
    # is never opened: it holds that project's secrets, and loading it would
    # also hand them to every shell command the agent runs.
    load_dotenv(Path(__file__).resolve().parent.parent / ".env")
    load_dotenv(get_data_dir() / ".env")  # installed copy: keep the key at ~/Library/Application Support/melon/.env, works from any folder

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
    if pi_mode is not None:  # --pi / --no-pi beat every config file; set here so pi's defaults fill in (or don't) to match
        config_dict["pi_mode"] = pi_mode

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
