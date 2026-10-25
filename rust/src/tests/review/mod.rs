//! Review pipeline, orchestrator, advisor, modes and TUI tests (ports of the matching tests/*.py files).
mod advisor;
mod json_mode;
mod orchestrator;
mod parallel_agents;
mod pi_mode;
mod pi_options;
mod pi_reads;
mod plan_offer;
mod review_commit_detect;
mod review_effort;
mod review_pipeline;
mod review_plausible;
mod review_removed_behaviour;
mod review_small_file;
mod review_sub_agent;
mod review_triage;
mod rpc_mode;
mod tui;

/// The sub-agent, worker and chat hooks and the output sink are process-wide, so tests that set them take turns.
pub(crate) use crate::ui::sink::TEST_LOCK as SERIAL;
