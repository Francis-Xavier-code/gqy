//! search — 会话轮次的排队、redo 与加载（自 src/state/conversation_db.rs 拆分）。

#![allow(
    clippy::redundant_closure,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

mod queued_prompts;
mod turns;
