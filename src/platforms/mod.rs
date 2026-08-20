use crate::agent::AgentMode;
use crate::config::{PlatformRateLimit, PromptAudience};

use crate::i18n::{text_for, Locale};
use crate::ipc::ImageAttachment;

use crate::state::PlatformSessionBindingKey;
use crate::web::{random_id, validate_content, ActorCommand, DaemonState, IpcRunGuard, RunInfo};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

mod registry;
pub(crate) use registry::*;
pub(crate) mod access_control;
mod adapters;
pub(crate) mod transports;
#[allow(unused_imports)]
pub(crate) use transports::*;
pub(crate) mod assets;
pub(crate) mod avatar;
pub(crate) mod commands;
pub(crate) mod onebot;
pub(crate) mod plugins;
mod tool;
mod types;
pub(crate) use adapters::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
