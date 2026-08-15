use super::access_control::{has_dynamic_access, AccessPermission};
use super::{
    commands, download_capped, markdown_to_plain, resolve_platform_session, run_platform_turn,
    sniff_image_mime, split_reply, BotGroupRole, BotSendAvailability, ConversationKind,
    ForwardNode, OutboundBody, OutboundMessage, OutboundOrigin, OutboundSegment, PartialSendError,
    PlatformAdapter, PlatformConversation, PlatformFollowupRun, PlatformGroupMember,
    PlatformImageData, PlatformInboundEvent, PlatformInboundEventKind, PlatformInboundMedia,
    PlatformMediaKind, PlatformMention, PlatformMessageInfo, PlatformMessagePosition,
    PlatformPrincipal, PlatformTurnContext, RateDecision, ResponseTarget, SendReceipt,
    TriggerDecision, TurnDispatch, TurnProfile,
};
use crate::config::{
    OneBotConfig, PlatformConversationKind, PlatformRateLimit, RealContextPluginSettings,
    REAL_CONTEXT_PLUGIN_ID,
};
use crate::i18n::text as t;
use crate::ipc::ImageAttachment;
use crate::state::{QueuedPromptAttachment, StateStore};
use crate::web::{
    clear_platform_session_content, enqueue_turn_update, random_id, reset_platform_persona_state,
    safe_error_message, DaemonState, PlatformPersonaResetError, PlatformSessionResetError,
    TurnUpdateMode, TurnUpdateRequest,
};
use anyhow::{bail, Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{
    header::{AUTHORIZATION, HOST},
    HeaderMap, StatusCode,
};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::future::{join_all, BoxFuture};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};
use tokio::task::JoinHandle;

mod core;
pub use core::*;
mod events;
pub use events::*;
mod handlers;
pub use handlers::*;
mod groups;
pub use groups::*;
mod messages;
pub use messages::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
#[cfg(test)]
mod tests4;
