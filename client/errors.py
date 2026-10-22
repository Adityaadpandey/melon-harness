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
        drop_max_tokens: bool = False,
        quota: bool = False,
    ):
        super().__init__(message)
        self.status = status
        self.retry = retry
        self.fallback = fallback
        self.drop_temperature = drop_temperature
        self.drop_max_tokens = drop_max_tokens  # the server refused our output cap (too big, or unknown field)
        self.quota = quota  # the whole provider/plan is out of quota: only another provider can help


# out of money or plan, whatever the status code: retrying the same provider can't help
QUOTA = re.compile(r"usage limit|limit resets|upgrade your plan|insufficient[_ ]quota|exceeded your current quota|insufficient (balance|credit)|out of credits|billing")
# busy or flaky upstream reported with an odd status (some open-model hosts send 400 or 422 for these)
TRANSIENT = re.compile(r"overloaded|temporarily unavailable|try again (later|shortly)|timed? ?out|rate.?limit|too many requests"
                       r"|connection (reset|refused|error|closed)|stream (ended|closed) unexpectedly|bad gateway|service unavailable")


def classify(status: int | None, message: str) -> ProviderError:
    m = message.lower()
    if status == 402 or (QUOTA.search(m) and status not in (400, 404)):
        # plan-wide quota, not a burst limit: retrying or switching model on the same provider can't help
        return ProviderError(message, status=status, fallback=True, quota=True)
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
    if TRANSIENT.search(m):
        return ProviderError(message, status=status, retry=True, fallback=True)
    if status == 400 and re.search(r"max_(completion_)?tokens", m):
        return ProviderError(message, status=status, drop_max_tokens=True)
    if status == 400 and re.search(r"temperature|top_p", m):
        return ProviderError(message, status=status, drop_temperature=True)
    return ProviderError(message, status=status)
