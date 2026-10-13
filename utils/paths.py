from pathlib import Path


def resolve_path(base: str | Path, path: str | Path):
    path = Path(path)
    if path.is_absolute():
        return path.resolve()

    return Path(base).resolve() / path


def display_path_rel_to_cwd(path: str, cwd: Path | None) -> str:
    try:
        p = Path(path)
    except Exception:
        return path

    if cwd:
        # resolve both sides: /var/... and /private/var/... name the same place on macOS
        for a, b in ((p, Path(cwd)), (p.resolve(), Path(cwd).resolve())):
            try:
                return str(a.relative_to(b))
            except ValueError:
                continue

    return str(p)


def ensure_parent_directory(path: str | Path) -> Path:
    path = Path(path)

    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def is_binary_file(path: str | Path) -> bool:
    try:
        with open(path, "rb") as f:
            chunk = f.read(8192)
            return b"\x00" in chunk
    except (OSError, IOError):
        return False
