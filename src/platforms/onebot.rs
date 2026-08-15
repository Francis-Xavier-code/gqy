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
    OneBotConfig, PlatformConversationKind, RealContextPluginSettings, REAL_CONTEXT_PLUGIN_ID,
};

use crate::i18n::text as t;
use crate::ipc::ImageAttachment;
use crate::state::{QueuedPromptAttachment, StateStore};
use crate::web::{
    clear_platform_session_content, enqueue_turn_update, reset_platform_persona_state,
    safe_error_message, DaemonState, PlatformPersonaResetError, PlatformSessionResetError,
    TurnUpdateMode, TurnUpdateRequest,
};

use anyhow::{bail, Context, Result};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::future::{join_all, BoxFuture};
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod core;
pub(crate) use core::*;
mod events;
pub(crate) use events::*;
mod handlers;
pub(crate) use handlers::*;
mod groups;
pub(crate) use groups::*;
mod messages;
pub(crate) use messages::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
#[cfg(test)]
mod tests4;
