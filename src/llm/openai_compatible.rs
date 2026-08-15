use super::{
    ChatMessage, ChatResult, ChatStreamChunk, ChatStreamKind, ResponsesContinuation, ToolCall,
    ToolCallFunction, ToolDefinition, Usage,
};
use crate::config::{AppConfig, ProviderConfig};
use crate::default_models::OPENCODE_ZEN_BASE_URL;
use crate::i18n::text as t;
use crate::models_cache::{self, ModelReasoningInfo, ReasoningSetting, ReasoningVariant};
use crate::paths::GQYPaths;
use anyhow::{bail, Context, Result};
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod core;
pub use core::*;
mod client_impl;
pub use client_impl::*;
mod client_impl2;
pub use client_impl2::*;
mod providers;
pub use providers::*;
mod providers2;
pub use providers2::*;
mod api;
pub use api::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
impl OpenAiCompatibleClient {}
