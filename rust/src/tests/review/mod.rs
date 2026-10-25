//! Review pipeline, orchestrator, advisor, modes and TUI tests (ports of the matching tests/*.py files).
mod orchestrator;
mod parallel_agents;
mod review_commit_detect;
mod review_effort;
mod review_pipeline;
mod review_plausible;
mod review_removed_behaviour;
mod review_small_file;
mod review_sub_agent;
mod review_triage;

/// The sub-agent and worker hooks are process-wide, so tests that set them take turns.
pub(crate) static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
