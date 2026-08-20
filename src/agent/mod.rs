use crate::clipboard::{ClipboardImage, PastedImage};
use crate::config::{AppConfig, PromptAudience};
use crate::host_info::xml_attr_escape;
use crate::llm::{
    ChatContent, ChatContentPart, ChatMessage, ChatResult, ChatStreamChunk, ChatStreamKind,
    ImageUrlContent, OpenAiCompatibleClient, ToolCall, ToolCallFunction, TurnTokens, Usage,
};
use crate::memory::{EvictedTurn, MemoryAccess, MemoryOrganizerHandle, MemoryOrigin, MemoryStore};
use crate::paths::GQYPaths;
use crate::persona_hint;
use crate::platforms::{PlatformContextImageRef, PlatformTurnContext};
use crate::question::{
    answered_tool_output, closed_tool_output, unavailable_tool_output, QuestionCancelled,
    QuestionExchange, QuestionRequest, QuestionResponse,
};
use crate::render::wait_spinner::SPINNER_INTERVAL;
use crate::state::{
    QueuedPrompt, QueuedPromptAttachment, RedoCandidate, RedoInputKind, StateStore,
    TurnRedoCheckpointPayload,
};
use crate::tools::{self, memes, vision, ToolRegistry};
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::Local;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

mod core;
pub(crate) use core::*;
mod compact;

mod conversation;

pub(crate) mod overflow;

mod lifecycle;

mod chat_stream;

mod overflow_handling;

mod tool_loop;

mod research;
pub(crate) use research::*;
mod tasks;
pub(crate) use tasks::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
#[cfg(test)]
mod tests4;
impl Agent {}
