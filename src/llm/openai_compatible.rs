use super::{
    ChatContent, ChatContentPart, ChatMessage, ChatResult, ChatStreamChunk, ChatStreamKind,
    ResponsesContinuation, ToolCall, ToolCallFunction, ToolDefinition, Usage,
};
use crate::config::{AppConfig, ProviderConfig};
use crate::default_models::OPENCODE_ZEN_BASE_URL;
use crate::i18n::text as t;
use crate::models_cache::{self, ModelReasoningInfo, ReasoningSetting, ReasoningVariant};
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use futures_util::{Stream, StreamExt};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

use std::sync::Arc;
use std::time::{Duration, Instant};

mod core;
pub(crate) use core::*;
mod client_impl;

mod client_impl2;

mod providers;
pub(crate) use providers::*;
mod providers2;
pub use providers2::*;
mod api;
pub(crate) use api::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
impl OpenAiCompatibleClient {}
