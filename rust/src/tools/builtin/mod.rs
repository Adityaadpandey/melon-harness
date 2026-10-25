//! The built-in tools.
pub mod advisor;
pub mod ask_user;
pub mod edit_file;
pub mod git;
pub mod glob;
pub mod graph;
pub mod grep;
pub mod list_dir;
pub mod memory;
pub mod notes;
pub mod outline;
pub mod parallel_tasks;
pub mod read_file;
pub mod recall;
pub mod search;
pub mod shell;
pub mod todo;
pub mod use_skill;
pub mod web_fetch;
pub mod web_search;
pub mod write_file;

use super::Tool;
use crate::config::ConfigHandle;
use std::sync::Arc;

/// The tools every session starts with, in the order their schemas are sent.
pub fn all_builtin(config: &ConfigHandle) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read_file::ReadFileTool::new(config.clone())),
        Arc::new(write_file::WriteFileTool),
        Arc::new(edit_file::EditTool),
        Arc::new(shell::ShellTool::new(config.clone())),
        Arc::new(list_dir::ListDirTool),
        Arc::new(grep::GrepTool::new(config.clone())),
        Arc::new(outline::OutlineTool),
        Arc::new(search::SearchTool::new(config.clone())),
        Arc::new(glob::GlobTool::new(config.clone())),
        Arc::new(git::GitTool),
        Arc::new(web_search::WebSearchTool),
        Arc::new(web_fetch::WebFetchTool::new(config.clone())),
        Arc::new(todo::TodosTool::default()),
        Arc::new(memory::MemoryTool::new(config.clone())),
        Arc::new(notes::NotesTool::new(config.clone())),
        Arc::new(use_skill::UseSkillTool::new(config.clone())),
        Arc::new(advisor::AdvisorTool::new(config.clone())),
        Arc::new(ask_user::AskUserTool::new(config.clone())),
    ]
}
