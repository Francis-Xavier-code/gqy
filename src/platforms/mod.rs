use crate::agent::{AgentMode, QueueIngressBarrier, QueueIngressReservation};
use crate::config::{
    ActiveProviderModelConfig, AppConfig, PlatformRateLimit, PlatformSessionLimits, PromptAudience,
};
use crate::i18n::{text_for, Locale};
use crate::ipc::ImageAttachment;
use crate::paths::GQYPaths;
use crate::state::{PlatformSessionBindingKey, StateStore};
use crate::web::{random_id, validate_content, ActorCommand, DaemonState, IpcRunGuard, RunInfo};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

mod registry;
pub use registry::*;
mod access_control;
mod adapters;
mod assets;
pub(crate) mod avatar;
pub(crate) mod commands;
pub(crate) mod onebot;
pub(crate) mod plugins;
mod tool;
mod types;
pub use adapters::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
