use crate::config::{
    merge_real_context_settings, ActiveProviderModelConfig, AppConfig, PlatformCommandPermission,
    PlatformConversationConfig, PlatformConversationKind, PlatformModelPoolInheritance,
    PlatformModelRoute, PlatformPersonaOverride, PlatformRateLimit, PlatformSessionLimits,
    ProviderConfig, ProviderModelChoice, QqMemeCollectorPluginSettings,
    QqMessageHistoryPluginSettings, RealContextIdentityMapping, RealContextPluginSettings,
    MAX_COMMAND_OUTPUT_LINES, MAX_PLATFORM_COMMAND_PREFIX_CHARS, MAX_PLATFORM_SESSION_QUEUED,
    MAX_PLATFORM_SESSION_RUNNING, MAX_REPL_REPLAY_TURNS, QQ_MEME_COLLECTOR_PLUGIN_ID,
    QQ_MESSAGE_HISTORY_PLUGIN_ID, REAL_CONTEXT_PLUGIN_ID,
};

use crate::default_models::{OPENCODE_DEFAULT_VISION_MODEL, OPENCODE_PROVIDER_ID};
use crate::i18n::{is_zh, text as t};
use crate::llm::{
    thinking_variant_options_for_model, ThinkingVariantOptions, ThinkingVariantPreferences,
};
use crate::paths::GQYPaths;
use crate::platforms::commands::{self, PlatformCommandDescriptor};
use crate::platforms::plugins::{
    active_judgement_skip_ids, apply_active_judgement_skip_editor_changes,
};
use crate::state::StateStore;
use anyhow::{bail, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

mod ui;
pub(crate) use ui::*;
mod plugins;
pub(crate) use plugins::*;
mod fields;
pub(crate) use fields::*;
mod fields2;
pub use fields2::*;
mod editors;
pub(crate) use editors::*;
mod menus;
pub(crate) use menus::*;
mod state;
pub(crate) use state::*;
#[cfg(test)]
mod tests;
