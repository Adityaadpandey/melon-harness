//! The conversation as the model sees it: messages, usage, pruning and compaction bookkeeping.
pub mod compaction;
pub mod loop_detector;
pub mod manager;
pub use manager::{ContextManager, MessageItem};
