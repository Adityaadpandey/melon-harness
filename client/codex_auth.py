"""Reuses the ChatGPT login that the Codex CLI already stored, so mew can call
Codex models with your subscription instead of an API key. We only read (and
refresh) ~/.codex/auth.json; we never ask you to log in again."""
from __future__ import annotations

import base64
import json
import os
import time
from pathlib import Path

import httpx

# the public client id the Codex CLI itself uses for token refresh
CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"
REFRESH_URL = "https://auth.openai.com/oauth/token"


def auth_path() -> Path:
    return Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")) / "auth.json"


def _expiry(jwt: str) -> float:
    """exp claim of a JWT, or 0 if it can't be read (treated as expired)."""
    try:
        body = jwt.split(".")[1]
        return float(json.loads(base64.urlsafe_b64decode(body + "=" * (-len(body) % 4)))["exp"])
    except (IndexError, KeyError, ValueError):
        return 0.0


class CodexAuth:
    def __init__(self, path: Path | None = None):
        self.path = path or auth_path()

    def _load(self) -> dict:
        try:
            data = json.loads(self.path.read_text())
        except (OSError, ValueError) as e:
            raise RuntimeError(f"No Codex login at {self.path}. Run `codex login` first.") from e
        if not data.get("tokens", {}).get("access_token"):
            raise RuntimeError("Codex is logged in with an API key, not ChatGPT. Run `codex login`.")
        return data

    def refresh(self, data: dict) -> dict:
        tokens = data["tokens"]
        r = httpx.post(
            REFRESH_URL,
            json={
                "client_id": CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": tokens["refresh_token"],
                "scope": "openid profile email",
            },
            timeout=20,
        )
        if r.status_code != 200:
            raise RuntimeError(f"Codex token refresh failed ({r.status_code}). Run `codex login` again.")
        new = r.json()
        for k in ("access_token", "refresh_token", "id_token"):
            if new.get(k):
                tokens[k] = new[k]
        data["last_refresh"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        # written back so the Codex CLI keeps working with the rotated refresh token
        self.path.write_text(json.dumps(data, indent=2))
        return data

    def credentials(self, force_refresh: bool = False) -> tuple[str, str]:
        """(access_token, account_id), refreshed first if it is about to expire."""
        data = self._load()
        if force_refresh or _expiry(data["tokens"]["access_token"]) < time.time() + 60:
            data = self.refresh(data)
        t = data["tokens"]
        return t["access_token"], t.get("account_id", "")
