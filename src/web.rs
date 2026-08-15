use crate::agent::{
    archive_and_delete_visible_turns, Agent, AgentEvent, AgentMode, AgentTurnControl,
};
use crate::cli::{build_tool_registry, WebArgs};
use crate::config::{ActiveProviderModelConfig, AppConfig, PromptAudience};
use crate::i18n::text as t;
use crate::ipc::{
    self, Command as IpcCommand, Frame as IpcFrame, ImageAttachment, Request as IpcRequest,
};
use crate::llm::{
    thinking_variant_options_for_model, ChatResult, ChatStreamKind, OpenAiCompatibleClient,
    ThinkingVariantOptions, ThinkingVariantPreferences, Usage,
};
use crate::memory::{
    MemoryAccess, MemoryOrganizer, MemoryOrganizerHandle, MemoryOrigin, MemoryStore,
};
use crate::paths::GQYPaths;
use crate::question::{self, QuestionAnswers, QuestionRequest, QuestionResponse};
use crate::state::{
    ArtifactAsset, ImageAsset, PlatformPluginScopeKey, QueuedPrompt, StateStore, Turn,
    TurnFollowup, TurnStatus, UsageSnapshot, UserAttachment,
};
use crate::tools::{self, CommandOutputStream};
use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE,
    COOKIE, HOST, ORIGIN, REFERRER_POLICY, RETRY_AFTER, SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::stream::{self, Stream};
use futures_util::StreamExt;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::future::IntoFuture;
use std::io::{self, IsTerminal, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path as FilePath, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, mpsc, oneshot, Semaphore};
use tokio::task::JoinHandle as TokioJoinHandle;

use crate::platforms::{self, PlatformRuntime};


mod defs;
use defs::*;
mod sessions_a;
use sessions_a::*;
mod sessions_b;
use sessions_b::*;
mod sessions2;
use sessions2::*;
mod config;
use config::*;
mod actors;
use actors::*;
mod state;
use state::*;
mod admin;
use admin::*;
mod assets;
use assets::*;
mod tests;
use tests::*;
mod tests2;
use tests2::*;
