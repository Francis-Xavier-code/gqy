//! core — 自 src/agent/mod.rs 拆分。

use super::*;

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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Notify};

pub(crate) const MAX_QUESTION_ROUNDS_PER_TURN: usize = 8;

pub struct PendingTurnGuard {
    state: StateStore,
    turn_id: String,
    completed: bool,
}

impl PendingTurnGuard {
    pub fn new(state: StateStore, turn_id: String) -> Self {
        Self {
            state,
            turn_id,
            completed: false,
        }
    }

    pub fn complete_with_model(
        mut self,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        self.state.complete_turn_with_usage_and_model(
            &self.turn_id,
            content,
            reasoning,
            provider_id,
            model,
            tokens,
            token_usage_estimated,
        )?;
        self.completed = true;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn interrupt(&mut self) -> Result<()> {
        if !self.completed {
            self.state.interrupt_turn(&self.turn_id)?;
            self.completed = true;
        }
        Ok(())
    }
}

impl Drop for PendingTurnGuard {
    pub(crate) fn drop(&mut self) {
        if !self.completed {
            if let Err(error) = self.state.interrupt_turn(&self.turn_id) {
                tracing::error!(
                    turn_id = %self.turn_id,
                    error = %error,
                    "failed to persist an interrupted turn"
                );
            }
        }
    }
}

pub(crate) struct PendingRedoGuard {
    state: StateStore,
    turn_id: String,
    revision: i64,
    completed: bool,
}

impl PendingRedoGuard {
    pub(crate) fn new(state: StateStore, turn_id: String, revision: i64) -> Self {
        Self {
            state,
            turn_id,
            revision,
            completed: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn complete_with_model(
        mut self,
        content: &str,
        reasoning: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        tokens: TurnTokens,
        token_usage_estimated: bool,
    ) -> Result<()> {
        self.state.complete_turn_revision_with_usage_and_model(
            &self.turn_id,
            self.revision,
            content,
            reasoning,
            provider_id,
            model,
            tokens,
            token_usage_estimated,
        )?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for PendingRedoGuard {
    pub(crate) fn drop(&mut self) {
        if !self.completed {
            if let Err(error) = self
                .state
                .interrupt_turn_revision(&self.turn_id, self.revision)
            {
                tracing::error!(
                    turn_id = %self.turn_id,
                    revision = self.revision,
                    error = %error,
                    "failed to recover an interrupted redo generation"
                );
            }
        }
    }
}

pub struct RedoPromptInput {
    pub prompt_id: String,
    pub content: String,
    pub display_content: String,
    pub images: Vec<Option<PastedImage>>,
}

/// 会话模式,创建时定死、中途不可切(切换=系统提示词换血=全量缓存作废)。
/// Normal=人格全能力;Dev=极简开发形态(一行可编辑提示词、无人格全家、
/// 精简工具目录)。原「闲聊(Chat)」模式已删除:平台路径从来只跑 Normal,
/// 安全靠 restricted registry(工具不存在)而非模式门,REPL 侧实测也无人用。
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AgentMode {
    Normal,
    Dev,
}

#[derive(Clone)]
pub struct AgentTurnControl {
    mode: Arc<Mutex<AgentMode>>,
    normal_tools: ToolRegistry,
    dev_tools: ToolRegistry,
    queue_ingress: Option<Arc<QueueIngressBarrier>>,
    supersede: Option<Arc<TurnSupersedeSignal>>,
    supersede_seen: Arc<AtomicU64>,
}

#[derive(Default)]
pub(crate) struct TurnSupersedeSignal {
    generation: AtomicU64,
    changed: Notify,
}

impl TurnSupersedeSignal {
    pub(crate) fn trigger(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.changed.notify_waiters();
        generation
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(crate) async fn wait_after(&self, observed: u64) {
        loop {
            let changed = self.changed.notified();
            if self.generation() != observed {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Default)]
pub(crate) struct QueueIngressBarrier {
    state: Mutex<QueueIngressState>,
    changed: Notify,
}

#[derive(Default)]
pub(crate) struct QueueIngressState {
    active_calls: HashSet<String>,
    reservations: usize,
    closed: bool,
}

pub(crate) struct QueueIngressReservation {
    barrier: Arc<QueueIngressBarrier>,
}

impl QueueIngressBarrier {
    pub(crate) fn tool_started(&self, call_id: &str) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.active_calls.insert(call_id.to_string());
        }
    }

    pub(crate) fn tool_finished(&self, call_id: &str) {
        self.state.lock().unwrap().active_calls.remove(call_id);
        self.changed.notify_waiters();
    }

    pub(crate) fn try_reserve(self: &Arc<Self>) -> Option<QueueIngressReservation> {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.active_calls.is_empty() {
            return None;
        }
        state.reservations = state.reservations.saturating_add(1);
        Some(QueueIngressReservation {
            barrier: self.clone(),
        })
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.active_calls.clear();
        self.changed.notify_waiters();
    }

    pub(crate) async fn wait_for_reserved_ingress(&self) {
        loop {
            let changed = self.changed.notified();
            if self.state.lock().unwrap().reservations == 0 {
                return;
            }
            changed.await;
        }
    }
}

impl Drop for QueueIngressReservation {
    pub(crate) fn drop(&mut self) {
        let mut state = self.barrier.state.lock().unwrap();
        state.reservations = state.reservations.saturating_sub(1);
        self.barrier.changed.notify_waiters();
    }
}

impl AgentTurnControl {
    pub fn new(mode: AgentMode, normal_tools: ToolRegistry, dev_tools: ToolRegistry) -> Self {
        Self {
            mode: Arc::new(Mutex::new(mode)),
            normal_tools,
            dev_tools,
            queue_ingress: None,
            supersede: None,
            supersede_seen: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn set_queue_ingress(&mut self, ingress: Arc<QueueIngressBarrier>) {
        self.queue_ingress = Some(ingress);
    }

    pub(crate) fn set_supersede_signal(&mut self, signal: Arc<TurnSupersedeSignal>) {
        self.supersede = Some(signal);
    }

    pub(crate) fn pending_supersede_generation(&self) -> Option<u64> {
        let generation = self.supersede.as_ref()?.generation();
        (generation != self.supersede_seen.load(Ordering::Acquire)).then_some(generation)
    }

    pub(crate) fn mark_supersede_seen(&self, generation: u64) {
        self.supersede_seen.store(generation, Ordering::Release);
    }

    pub fn mode(&self) -> AgentMode {
        *self.mode.lock().unwrap()
    }

    pub fn set_mode(&self, mode: AgentMode) {
        *self.mode.lock().unwrap() = mode;
    }

    pub(crate) fn tools(&self, mode: AgentMode) -> ToolRegistry {
        match mode {
            AgentMode::Normal => self.normal_tools.clone(),
            AgentMode::Dev => self.dev_tools.clone(),
        }
    }
}

impl AgentMode {
    pub fn label(self) -> &'static str {
        if crate::i18n::is_zh() {
            match self {
                Self::Normal => "普通",
                Self::Dev => "开发",
            }
        } else {
            match self {
                Self::Normal => "NORMAL",
                Self::Dev => "DEV",
            }
        }
    }

    pub(crate) fn reminder(self) -> Option<&'static str> {
        // Dev 遵循极简原则:不注入任何模式提醒。
        match self {
            Self::Normal | Self::Dev => None,
        }
    }
}

#[derive(Debug)]
pub enum AgentEvent {
    TurnStarted {
        turn_id: String,
    },
    Chunk(ChatStreamChunk),
    /// Raw provider reasoning, persisted before the UI title/body filter.
    /// This event is consumed by `TurnJournalSink` and is never shown to a
    /// transport directly.
    RawReasoning(ChatStreamChunk),
    /// Internal durability barrier used before non-stream state mutations that
    /// create journal boundaries.
    FlushJournal,
    ReasoningStart {
        received_at: Instant,
    },
    ReasoningReset {
        received_at: Instant,
    },
    ReasoningPartStart {
        received_at: Instant,
    },
    ReasoningPartEnd {
        received_at: Instant,
    },
    ReasoningTitle(String),
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolPreparing {
        name: String,
    },
    ToolResult {
        call_id: String,
        name: String,
        ok: bool,
        output: String,
    },
    ToolProgress {
        call_id: String,
        name: String,
        message: String,
    },
    CommandOutput {
        call_id: String,
        name: String,
        stream: tools::CommandOutputStream,
        chunk: Vec<u8>,
    },
    PrepareForExternalOutput {
        ready: oneshot::Sender<bool>,
    },
    Image {
        call_id: String,
        name: String,
        path: PathBuf,
        alt: String,
    },
    Artifact {
        call_id: String,
        name: String,
        path: PathBuf,
        title: String,
    },
    AskQuestion {
        call_id: String,
        request: QuestionRequest,
        responder: oneshot::Sender<QuestionResponse>,
    },
    QueuedPromptsConsumed {
        prompt_ids: Vec<String>,
        mode: AgentMode,
        provider_id: Option<String>,
        model: Option<String>,
    },
    GenerationSuperseded {
        prompt_ids: Vec<String>,
    },
    /// 回合内每次模型请求结束时的用量快照:`round` 是刚结束这次请求的
    /// 用量(其 prompt+completion ≈ 当前上下文占用),`turn` 是回合开始
    /// 至今的累计。终端 footer 和 WebUI 用它逐请求刷新计量,不必等整个
    /// 回合(可能含多轮工具调用)结束。
    RoundUsage {
        round: Box<Usage>,
        turn: TurnTokens,
        estimated: bool,
    },
    SpinnerTick,
    CompactStart,
    CompactChunk(ChatStreamChunk),
    CompactEnd,
    PopStart,
    PopEnd,
    /// One-shot operational notice shown to the user (e.g. auto-compaction
    /// paused because the window is too small).
    Notice {
        text: String,
    },
}

pub(crate) const JOURNAL_FLUSH_BYTES: usize = 16 * 1024;
pub(crate) const JOURNAL_FLUSH_INTERVAL: Duration = Duration::from_millis(80);

pub(crate) struct PendingJournalChunk {
    kind: ChatStreamKind,
    text: String,
}

/// Persists semantic stream events before forwarding them to a transport.
/// Small adjacent deltas are coalesced so a long answer does not turn into a
/// SQLite transaction per provider token.
pub(crate) struct TurnJournalSink {
    state: StateStore,
    turn_id: String,
    revision: i64,
    segment_index: i64,
    pending: Option<PendingJournalChunk>,
    pending_reasoning_display: String,
    last_flush: Instant,
}

impl TurnJournalSink {
    pub(crate) fn new(state: StateStore, turn_id: String, revision: i64) -> Self {
        Self {
            state,
            turn_id,
            revision,
            segment_index: 0,
            pending: None,
            pending_reasoning_display: String::new(),
            last_flush: Instant::now(),
        }
    }

    pub(crate) fn emit<F>(&mut self, event: AgentEvent, on_event: &mut F) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        match event {
            AgentEvent::Chunk(chunk)
                if matches!(
                    chunk.kind,
                    ChatStreamKind::Content | ChatStreamKind::ToolCall
                ) =>
            {
                self.push_chunk(chunk, on_event)
            }
            AgentEvent::Chunk(chunk) if chunk.kind == ChatStreamKind::Reasoning => {
                self.pending_reasoning_display.push_str(&chunk.text);
                Ok(())
            }
            AgentEvent::RawReasoning(chunk) => {
                if chunk.kind == ChatStreamKind::Reasoning && !chunk.text.is_empty() {
                    self.push_chunk(chunk, on_event)
                } else {
                    Ok(())
                }
            }
            AgentEvent::FlushJournal => self.flush(on_event),
            AgentEvent::SpinnerTick => {
                self.flush(on_event)?;
                on_event(AgentEvent::SpinnerTick)
            }
            // 瞬态计量快照,只给 UI,不入回放日志。
            event @ AgentEvent::RoundUsage { .. } => on_event(event),
            AgentEvent::ReasoningStart { received_at } => {
                self.flush(on_event)?;
                self.append("reasoning_start", None, None, None, None, None)?;
                on_event(AgentEvent::ReasoningStart { received_at })
            }
            AgentEvent::ReasoningReset { received_at } => {
                self.flush(on_event)?;
                self.append("reasoning_reset", None, None, None, None, None)?;
                on_event(AgentEvent::ReasoningReset { received_at })
            }
            AgentEvent::ReasoningPartStart { received_at } => {
                self.flush(on_event)?;
                self.append("reasoning_part_start", None, None, None, None, None)?;
                on_event(AgentEvent::ReasoningPartStart { received_at })
            }
            AgentEvent::ReasoningPartEnd { received_at } => {
                self.flush(on_event)?;
                self.append("reasoning_part_end", None, None, None, None, None)?;
                on_event(AgentEvent::ReasoningPartEnd { received_at })
            }
            AgentEvent::ReasoningTitle(title) => {
                self.flush(on_event)?;
                self.append("reasoning_title", None, None, Some(&title), None, None)?;
                on_event(AgentEvent::ReasoningTitle(title))
            }
            AgentEvent::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                self.flush(on_event)?;
                self.append(
                    "tool_call",
                    Some(&call_id),
                    Some(&name),
                    Some(&arguments),
                    None,
                    None,
                )?;
                on_event(AgentEvent::ToolCall {
                    call_id,
                    name,
                    arguments,
                })
            }
            AgentEvent::ToolPreparing { name } => {
                self.flush(on_event)?;
                self.append("tool_preparing", None, Some(&name), Some(&name), None, None)?;
                on_event(AgentEvent::ToolPreparing { name })
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                ok,
                output,
            } => {
                self.flush(on_event)?;
                self.append(
                    "tool_result",
                    Some(&call_id),
                    Some(&name),
                    Some(&output),
                    None,
                    Some(ok),
                )?;
                on_event(AgentEvent::ToolResult {
                    call_id,
                    name,
                    ok,
                    output,
                })
            }
            AgentEvent::ToolProgress {
                call_id,
                name,
                message,
            } => {
                self.flush(on_event)?;
                self.append(
                    "tool_progress",
                    Some(&call_id),
                    Some(&name),
                    Some(&message),
                    None,
                    None,
                )?;
                on_event(AgentEvent::ToolProgress {
                    call_id,
                    name,
                    message,
                })
            }
            AgentEvent::CommandOutput {
                call_id,
                name,
                stream,
                chunk,
            } => {
                self.flush(on_event)?;
                let kind = match stream {
                    tools::CommandOutputStream::Stdout => "command_stdout",
                    tools::CommandOutputStream::Stderr => "command_stderr",
                };
                self.append(kind, Some(&call_id), Some(&name), None, Some(&chunk), None)?;
                on_event(AgentEvent::CommandOutput {
                    call_id,
                    name,
                    stream,
                    chunk,
                })
            }
            AgentEvent::Image {
                call_id,
                name,
                path,
                alt,
            } => {
                self.flush(on_event)?;
                let payload = serde_json::json!({
                    "path": path.display().to_string(),
                    "alt": alt,
                });
                let payload = serde_json::to_string(&payload)?;
                self.append(
                    "image",
                    Some(&call_id),
                    Some(&name),
                    Some(&payload),
                    None,
                    None,
                )?;
                on_event(AgentEvent::Image {
                    call_id,
                    name,
                    path,
                    alt,
                })
            }
            AgentEvent::Artifact {
                call_id,
                name,
                path,
                title,
            } => {
                self.flush(on_event)?;
                let payload = serde_json::json!({
                    "path": path.display().to_string(),
                    "title": title,
                });
                let payload = serde_json::to_string(&payload)?;
                self.append(
                    "artifact",
                    Some(&call_id),
                    Some(&name),
                    Some(&payload),
                    None,
                    None,
                )?;
                on_event(AgentEvent::Artifact {
                    call_id,
                    name,
                    path,
                    title,
                })
            }
            AgentEvent::AskQuestion {
                call_id,
                request,
                responder,
            } => {
                self.flush(on_event)?;
                let payload = serde_json::to_string(&request)?;
                self.append(
                    "question",
                    Some(&call_id),
                    Some("ask_question"),
                    Some(&payload),
                    None,
                    None,
                )?;
                on_event(AgentEvent::AskQuestion {
                    call_id,
                    request,
                    responder,
                })
            }
            AgentEvent::GenerationSuperseded { prompt_ids } => {
                self.flush(on_event)?;
                self.state.supersede_turn_journal_segment(
                    &self.turn_id,
                    self.revision,
                    self.segment_index,
                )?;
                on_event(AgentEvent::GenerationSuperseded { prompt_ids })
            }
            AgentEvent::QueuedPromptsConsumed {
                prompt_ids,
                mode,
                provider_id,
                model,
            } => {
                self.flush(on_event)?;
                self.segment_index = self.segment_index.saturating_add(1);
                on_event(AgentEvent::QueuedPromptsConsumed {
                    prompt_ids,
                    mode,
                    provider_id,
                    model,
                })
            }
            AgentEvent::CompactStart
            | AgentEvent::CompactChunk(_)
            | AgentEvent::CompactEnd
            | AgentEvent::PopStart
            | AgentEvent::PopEnd
            | AgentEvent::Notice { .. }
            | AgentEvent::TurnStarted { .. }
            | AgentEvent::PrepareForExternalOutput { .. } => on_event(event),
            AgentEvent::Chunk(chunk) => on_event(AgentEvent::Chunk(chunk)),
        }
    }

    pub(crate) fn push_chunk<F>(&mut self, chunk: ChatStreamChunk, on_event: &mut F) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        if self.pending.is_none() && !self.pending_reasoning_display.is_empty() {
            self.flush(on_event)?;
        }
        let should_flush = self.pending.as_ref().is_some_and(|pending| {
            pending.kind != chunk.kind
                || pending.text.len().saturating_add(chunk.text.len()) >= JOURNAL_FLUSH_BYTES
                || self.last_flush.elapsed() >= JOURNAL_FLUSH_INTERVAL
        });
        if should_flush {
            self.flush(on_event)?;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.text.push_str(&chunk.text);
        } else {
            self.pending = Some(PendingJournalChunk {
                kind: chunk.kind,
                text: chunk.text,
            });
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.text.len() >= JOURNAL_FLUSH_BYTES)
        {
            self.flush(on_event)?;
        }
        Ok(())
    }

    pub(crate) fn flush<F>(&mut self, on_event: &mut F) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        let Some(pending) = self.pending.take() else {
            if self.pending_reasoning_display.is_empty() {
                return Ok(());
            }
            let text = std::mem::take(&mut self.pending_reasoning_display);
            on_event(AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text,
            }))?;
            self.last_flush = Instant::now();
            return Ok(());
        };
        let kind = match pending.kind {
            ChatStreamKind::Content => "assistant_content",
            ChatStreamKind::Reasoning => "assistant_reasoning",
            ChatStreamKind::ToolCall => "tool_call_delta",
            ChatStreamKind::ReasoningReset
            | ChatStreamKind::ReasoningPartStart
            | ChatStreamKind::ReasoningPartEnd => return Ok(()),
        };
        self.append(kind, None, None, Some(&pending.text), None, None)?;
        self.last_flush = Instant::now();
        if pending.kind == ChatStreamKind::Reasoning {
            let text = std::mem::take(&mut self.pending_reasoning_display);
            if text.is_empty() {
                return Ok(());
            }
            return on_event(AgentEvent::Chunk(ChatStreamChunk {
                kind: ChatStreamKind::Reasoning,
                text,
            }));
        }
        on_event(AgentEvent::Chunk(ChatStreamChunk {
            kind: pending.kind,
            text: pending.text,
        }))
    }

    pub(crate) fn finish<F>(&mut self, on_event: &mut F) -> Result<()>
    where
        F: FnMut(AgentEvent) -> Result<()>,
    {
        self.flush(on_event)
    }

    pub(crate) fn append(
        &self,
        kind: &str,
        call_id: Option<&str>,
        name: Option<&str>,
        text_payload: Option<&str>,
        blob_payload: Option<&[u8]>,
        ok: Option<bool>,
    ) -> Result<()> {
        self.state.append_turn_journal_event(
            &self.turn_id,
            self.revision,
            self.segment_index,
            kind,
            call_id,
            name,
            text_payload,
            blob_payload,
            ok,
        )
    }
}

pub(crate) fn emit_tool_progress<F>(
    on_event: &mut F,
    call_id: &str,
    name: &str,
    progress: tools::ToolProgressEvent,
) -> Result<()>
where
    F: FnMut(AgentEvent) -> Result<()>,
{
    match progress {
        tools::ToolProgressEvent::Message(message) => on_event(AgentEvent::ToolProgress {
            call_id: call_id.to_string(),
            name: name.to_string(),
            message,
        }),
        tools::ToolProgressEvent::PrepareForExternalOutput { ready } => {
            on_event(AgentEvent::PrepareForExternalOutput { ready })
        }
        tools::ToolProgressEvent::Image { path, alt } => on_event(AgentEvent::Image {
            call_id: call_id.to_string(),
            name: name.to_string(),
            path,
            alt,
        }),
        tools::ToolProgressEvent::Artifact { path, title } => on_event(AgentEvent::Artifact {
            call_id: call_id.to_string(),
            name: name.to_string(),
            path,
            title,
        }),
        tools::ToolProgressEvent::CommandOutput { stream, chunk } => {
            on_event(AgentEvent::CommandOutput {
                call_id: call_id.to_string(),
                name: name.to_string(),
                stream,
                chunk,
            })
        }
    }
}

pub struct Agent {
    state: StateStore,
    client: OpenAiCompatibleClient,
    system_prompt: String,
    /// Per-run system additions supplied by a transport/plugin. They are
    /// intentionally excluded from prompt-change hashing and persistence.
    runtime_system_context: Vec<String>,
    /// Per-message transport context (sender identity JSON, message ids, …)
    /// rendered as a tail system message after the user turn. Kept out of the
    /// system prompt so the stable prefix stays byte-identical across turns.
    turn_system_context: Vec<String>,
    /// Raw user input snapshot taken before platform plugins wrapped the turn
    /// content (instruction boilerplate, group history, …). The memory diary
    /// records this instead of the wrapped prompt — the minimal C10 "记忆只读
    /// raw_content" separation. `None` on paths whose input is already raw
    /// (terminal, WebUI) and on redo replays.
    memory_content: Option<String>,
    suppress_session_history: bool,
    trim_at_ratio: f32,
    trim_batch_ratio: f32,
    tools_enabled: bool,
    max_tool_rounds: usize,
    tools: Arc<Mutex<ToolRegistry>>,
    memory: MemoryStore,
    memory_organizer: Option<MemoryOrganizerHandle>,
    memory_origin: MemoryOrigin,
    memory_database_id: String,
    memory_generation: i64,
    mode: AgentMode,
    prompt_audience: PromptAudience,
    config: AppConfig,
    paths: GQYPaths,
    on_overflow: String,
    turn_display_content: Option<String>,
    attachment_run_id: Option<String>,
    image_platform: Option<String>,
    image_platform_label: Option<String>,
    platform_context: Option<Arc<PlatformTurnContext>>,
    context_images: Vec<PlatformContextImageRef>,
    /// 本回合的浮动尾部人格提醒全文。只追加进发送副本
    /// `request_messages`,永不进 `messages`,因此不化石化、不落库——
    /// 见 persona_hint 模块头注释。
    persona_reminder: Option<String>,
    /// 重复调用链(advisory 防死循环,见 tools::repeat_reminder 模块头)。
    /// 人类新输入(新回合/排队插话)重置;注入的提醒只进本轮工作消息,
    /// 不进化石。
    repeat_chain: crate::tools::repeat_reminder::RepeatChain,
    /// 预设对话(begin_dialogs):system 之后、真实历史之前的 user/assistant
    /// 示例对,每请求注入、永不落库。构造时从当前人格 scope 的
    /// dialogs/<scope>.md 加载。
    preset_dialogs: Vec<(String, String)>,
    /// Exact (messages, tools) of the most recent live request; feeds the
    /// idle cache-keepalive pings (v7 DeepSeek 高命中策略). Only populated
    /// while `cache.keepalive_seconds > 0`.
    last_request_snapshot: Option<(Vec<ChatMessage>, Vec<crate::llm::ToolDefinition>)>,
    /// 上一条真实请求最终落在哪个 endpoint(provider_id, model):keepalive
    /// ping 必须钉住同一缓存域,轮转调度下打到别家=白花钱不保温
    /// (deepseek 报告 P2)。
    last_request_endpoint: Option<(String, String)>,
    /// Cancels the currently running keepalive loop, if any.
    keepalive_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Consecutive auto-compactions that failed to bring the context back
    /// under the trigger. A healthy compaction lands below the trigger; two
    /// in a row mean the verbatim floor alone exceeds it (window too small),
    /// so auto-compaction latches off until the context drops (`compact_stuck`).
    consecutive_compacts: std::sync::atomic::AtomicU32,
    compact_stuck: std::sync::atomic::AtomicBool,
    /// Max turn seq observed right after the previous auto-compaction (-1 =
    /// none yet). A new compaction firing within a few turns of the last one
    /// means some single item (a huge paste or tool output) refills the
    /// window instantly — compacting harder won't help ("thrashing").
    last_compact_max_seq: std::sync::atomic::AtomicI64,
    rapid_compacts: std::sync::atomic::AtomicU32,
    /// One-shot "context is getting large" notice at the soft watermark.
    soft_notice_sent: std::sync::atomic::AtomicBool,
}

pub(crate) struct PreparedUserInput {
    content: String,
    message: ChatMessage,
    hints: Vec<ChatMessage>,
}

/// Output of a `task` call executed in the parallel group.
pub(crate) struct GroupTaskOutput {
    output: String,
    /// Persistable tool report, extracted at completion.
    report: Option<String>,
}
