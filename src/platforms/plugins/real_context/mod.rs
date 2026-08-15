pub(super) mod active_judgement_skip;

mod core;
use core::*;
mod plugin_impl;
use plugin_impl::*;
mod platform_impl;
use platform_impl::*;
mod trigger;
use trigger::*;
mod runtime;
use runtime::*;
mod gates;
use gates::*;
mod affection;
mod judge;
#[cfg(test)]
mod tests;

use super::message_history::{self, store, ORIGINAL_TEXT_KEY};
use super::{
    PlatformPersonaResetContext, PlatformPlugin, PlatformTurnInput, PluginDescriptor, PreparedSend,
};
use crate::config::{
    PlatformPluginInstanceConfig, RealContextPluginSettings, REAL_CONTEXT_PLUGIN_ID,
};
use crate::i18n::{text_for, Locale};
use crate::platforms::{
    AdaptiveResponseTargetPolicy, BotSendAvailability, ConversationKind, OutboundBody,
    OutboundMessage, OutboundOrigin, OutboundSegment, PlatformInboundEvent,
    PlatformInboundEventKind, PlatformMediaKind, PlatformMention, PlatformTurnContext,
    ResponseTarget, SendReceipt, TriggerDecision,
};
use crate::tools::ToolRegistry;
use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use store::{
    AccountKey, GroupKey, HistoryMessage, HistoryStore, MediaKind, MediaPlaceholder, RecentQuery,
};
#[cfg(test)]
use store::{NewHistoryMessage, SanitizedContent};
use tokio::sync::Notify;
impl RealContextPlugin {}

impl PlatformPlugin for RealContextPlugin {}
