//! Review pipeline, orchestrator, advisor, modes and TUI tests (ports of the matching tests/*.py files).
mod commit_detect;
mod effort;
mod plausible;
mod removed_behaviour;
mod small_file;
mod sub_agent;
mod triage;

/// The sub-agent and worker hooks are process-wide, so tests that set them take turns.
pub(crate) static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
