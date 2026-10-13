from __future__ import annotations
import re


class ProviderError(Exception):
    """A model call failed. `retry` = same model may work shortly (rate limit,
    5xx, network). `fallback` = this model won't work for us right now (not in
    plan, unknown model, no upstream), so try another one. Neither = the
    request itself is bad and another model won't help."""

    def __init__(
        self,
        message: str,
        *,
        status: int | None = None,
        retry: bool = False,
        fallback: bool = False,
        drop_temperature: bool = False,
    ):
        super().__init__(message)
        self.status = status
        self.retry = retry
        self.fallback = fallback
        self.drop_temperature = drop_temperature


def classify(status: int | None, message: str) -> ProviderError:
    m = message.lower()
    if status == 429:
        return ProviderError(message, status=status, retry=True, fallback=True)
    if status is None or status >= 500 or status == 408:
        return ProviderError(message, status=status, retry=True, fallback=True)
    if status == 403 and "model_not_in_plan" in m:
        return ProviderError(message, status=status, fallback=True)
    if status in (401, 403):
        return ProviderError(message, status=status)
    if status == 404 or "no available providers" in m or "unsupported_model" in m:
        return ProviderError(message, status=status, fallback=True)
    if status == 400 and re.search(r"temperature|top_p", m):
        return ProviderError(message, status=status, drop_temperature=True)
    return ProviderError(message, status=status)
