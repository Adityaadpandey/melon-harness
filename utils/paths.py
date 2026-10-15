from pathlib import Path


def resolve_path(base: str | Path, path: str | Path):
    path = Path(path).expanduser()  # skill locations are shown as ~/... to keep the prompt short
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


def atomic_write(path: str | Path, text: str) -> None:
    """Write via a temp file in the same folder and rename over the target, so a
    crash or kill mid-write leaves the old file intact instead of a truncated one.
    Keeps the original permissions, and writes through a symlink to its target."""
    import os
    import tempfile

    path = Path(path).resolve()
    mode = path.stat().st_mode & 0o777 if path.exists() else None
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as f:  # newline="": keep CRLF as given
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        if mode is not None:
            os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise
