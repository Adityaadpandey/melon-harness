"""Explicit lifecycle for one user request. Transitions are validated so a run
can't, say, jump from FAILED to VERIFIED, and every move is traced."""
from __future__ import annotations

import time
import uuid
from dataclasses import dataclass, field
from enum import Enum

from utils.trace import Tracer


class TaskStatus(str, Enum):
    RUNNING = "running"
    VERIFYING = "verifying"
    REPAIRING = "repairing"
    # terminal
    DONE_NO_CHANGES = "done_no_changes"  # nothing was modified, nothing to verify
    VERIFIED = "verified"  # changes made and every check passed
    UNVERIFIED = "unverified"  # changes made but no checks were available
    VERIFICATION_FAILED = "verification_failed"  # checks still failing after repairs
    FAILED = "failed"  # API error, loop, budget or deadline


TERMINAL = {
    TaskStatus.DONE_NO_CHANGES,
    TaskStatus.VERIFIED,
    TaskStatus.UNVERIFIED,
    TaskStatus.VERIFICATION_FAILED,
    TaskStatus.FAILED,
}

_ALLOWED = {
    TaskStatus.RUNNING: {
        TaskStatus.VERIFYING,
        TaskStatus.REPAIRING,  # review findings on a run that had no checks
        TaskStatus.DONE_NO_CHANGES,
        TaskStatus.UNVERIFIED,
        TaskStatus.FAILED,
    },
    TaskStatus.VERIFYING: {
        TaskStatus.VERIFIED,
        TaskStatus.REPAIRING,
        TaskStatus.VERIFICATION_FAILED,
        TaskStatus.FAILED,
    },
    TaskStatus.REPAIRING: {
        TaskStatus.REPAIRING,  # reviewer finds issues in a run that is already being repaired
        TaskStatus.VERIFYING,
        TaskStatus.UNVERIFIED,  # review-triggered repair on a run with no checks
        TaskStatus.DONE_NO_CHANGES,
        TaskStatus.FAILED,
    },
}


class InvalidTransition(Exception):
    pass


@dataclass
class TaskState:
    request: str
    tracer: Tracer
    max_seconds: float
    max_repairs: int
    id: str = field(default_factory=lambda: uuid.uuid4().hex[:8])
    status: TaskStatus = TaskStatus.RUNNING
    repairs: int = 0
    changed_files: list[str] = field(default_factory=list)
    history: list[tuple[float, str, str]] = field(default_factory=list)
    verification: list[dict] = field(default_factory=list)  # one entry per check run
    review: dict | None = None  # {verdict, findings} from the independent reviewer
    grounding: dict | None = None  # {checked, unverified[]}: file references in the final answer vs the real tree
    started: float = field(default_factory=time.monotonic)
    paused: float = 0.0  # seconds spent waiting for the user (approval prompts), left out of the time budget

    def __post_init__(self) -> None:
        self.tracer.emit("task_start", task=self.id, request=self.request[:300])

    @property
    def terminal(self) -> bool:
        return self.status in TERMINAL

    def remaining(self) -> float:
        return max(0.0, self.max_seconds - (time.monotonic() - self.started - self.paused))

    def expired(self) -> bool:
        return self.remaining() <= 0

    def transition(self, to: TaskStatus, note: str = "") -> None:
        if to not in _ALLOWED.get(self.status, set()):
            raise InvalidTransition(f"{self.status.value} -> {to.value}")
        self.history.append((round(time.monotonic() - self.started, 2), to.value, note))
        self.tracer.emit(
            "task_transition", task=self.id, frm=self.status.value, to=to.value, note=note
        )
        self.status = to
        if to == TaskStatus.REPAIRING:
            self.repairs += 1
        if self.terminal:
            self.tracer.emit(
                "task_end",
                task=self.id,
                status=to.value,
                repairs=self.repairs,
                sec=round(time.monotonic() - self.started, 2),
                changed=self.changed_files,
            )

    def report(self) -> dict:
        return {
            "task": self.id,
            "status": self.status.value,
            "repairs": self.repairs,
            "changed_files": self.changed_files,
            "checks": self.verification,
            "review": self.review,
            "grounding": self.grounding,
        }
