//! Explicit lifecycle for one user request. Transitions are validated so a run can't, say, jump from FAILED to
//! VERIFIED, and every move is traced.
use crate::util::trace::Tracer;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Verifying,
    Repairing,
    /// Nothing was modified, nothing to verify.
    DoneNoChanges,
    /// Changes made and every check passed.
    Verified,
    /// Changes made but no checks were available.
    Unverified,
    /// Checks still failing after repairs.
    VerificationFailed,
    /// API error, loop, budget or deadline.
    Failed,
}

impl TaskStatus {
    pub fn value(&self) -> &'static str {
        match self {
            TaskStatus::Running => "running",
            TaskStatus::Verifying => "verifying",
            TaskStatus::Repairing => "repairing",
            TaskStatus::DoneNoChanges => "done_no_changes",
            TaskStatus::Verified => "verified",
            TaskStatus::Unverified => "unverified",
            TaskStatus::VerificationFailed => "verification_failed",
            TaskStatus::Failed => "failed",
        }
    }
    pub fn terminal(&self) -> bool {
        !matches!(self, TaskStatus::Running | TaskStatus::Verifying | TaskStatus::Repairing)
    }
    pub(crate) fn allowed(&self, to: TaskStatus) -> bool {
        use TaskStatus::*;
        match self {
            Running => matches!(to, Verifying | Repairing | DoneNoChanges | Unverified | Failed), // Repairing: review findings on a run that had no checks
            Verifying => matches!(to, Running | Verified | Unverified | Repairing | VerificationFailed | Failed), // Running: the user steered mid-check
            Repairing => matches!(to, Repairing | Verifying | Unverified | DoneNoChanges | Failed),
            _ => false,
        }
    }
}

pub struct TaskState {
    pub request: String,
    tracer: Arc<Tracer>,
    pub max_seconds: f64,
    pub max_repairs: usize,
    pub id: String,
    pub status: TaskStatus,
    pub repairs: usize,
    pub changed_files: Vec<String>,
    pub history: Vec<(f64, String, String)>,
    /// One entry per check run.
    pub verification: Vec<Value>,
    /// {verdict, findings} from the independent reviewer.
    pub review: Option<Value>,
    /// {checked, unverified[]}: file references in the final answer vs the real tree.
    pub grounding: Option<Value>,
    pub started: Instant,
    /// Seconds spent waiting for the user (approval prompts), left out of the time budget.
    pub paused: f64,
}

impl TaskState {
    pub fn new(request: &str, tracer: Arc<Tracer>, max_seconds: f64, max_repairs: usize) -> TaskState {
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        tracer.emit("task_start", json!({"task": id, "request": crate::util::text::head_chars(request, 300)}));
        TaskState {
            request: request.into(),
            tracer,
            max_seconds,
            max_repairs,
            id,
            status: TaskStatus::Running,
            repairs: 0,
            changed_files: vec![],
            history: vec![],
            verification: vec![],
            review: None,
            grounding: None,
            started: Instant::now(),
            paused: 0.0,
        }
    }

    pub fn remaining(&self) -> f64 {
        (self.max_seconds - (self.started.elapsed().as_secs_f64() - self.paused)).max(0.0)
    }

    pub fn expired(&self) -> bool {
        self.remaining() <= 0.0
    }

    pub fn transition(&mut self, to: TaskStatus, note: &str) {
        if !self.status.allowed(to) {
            // an invalid move is a bug in the loop, not something the user can act on: record it and carry on
            self.tracer.emit("task_invalid_transition", json!({"task": self.id, "from": self.status.value(), "to": to.value()}));
            return;
        }
        let at = (self.started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
        self.history.push((at, to.value().into(), note.into()));
        self.tracer.emit("task_transition", json!({"task": self.id, "frm": self.status.value(), "to": to.value(), "note": note}));
        self.status = to;
        if to == TaskStatus::Repairing {
            self.repairs += 1;
        }
        if self.status.terminal() {
            self.tracer.emit("task_end", json!({"task": self.id, "status": to.value(), "repairs": self.repairs, "sec": at, "changed": self.changed_files}));
        }
    }

    pub fn report(&self) -> Value {
        json!({"task": self.id, "status": self.status.value(), "repairs": self.repairs, "changed_files": self.changed_files,
               "checks": self.verification, "review": self.review, "grounding": self.grounding})
    }
}
