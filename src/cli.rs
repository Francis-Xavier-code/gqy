use crate::agent::{
    archive_and_delete_visible_turns, Agent, AgentEvent, AgentMode, AgentTurnControl,
};
use crate::config::{ActiveProviderModelConfig, AppConfig};
use crate::i18n::{is_zh, text as t};
use crate::ipc::{self, Command as IpcCommand, Frame as IpcFrame, Request as IpcRequest};
use crate::llm::{
    ChatResult, ChatStreamChunk, OpenAiCompatibleClient, ThinkingVariantOptions, TurnTokens, Usage,
};
use crate::memory::{MemoryOrganizer, MemoryStore};
use crate::paths::GQYPaths;
use crate::render;
use crate::shell;
use crate::state::{QueuedPrompt, QueuedPromptAttachment, StateStore, Turn, TurnStatus};
use crate::tools;
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::{DateTime, Local};
use clap::{Arg, ArgAction, Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use crossterm::cursor::{self, Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::style::{Color, Print, Stylize};
use crossterm::terminal::{self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate};
use crossterm::{execute, queue};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::ffi::OsString;
use std::io::Cursor;
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use vte::{Params as VteParams, Parser as VteParser, Perform as VtePerform};

mod defs;
use defs::*;
mod daemon;
use daemon::*;
mod init;
use init::*;
mod fuzzy;
use fuzzy::*;
mod oneshot;
use oneshot::*;
mod ipc;
use ipc::*;
mod remote;
use remote::*;
mod direct;
use direct::*;
mod variant;
use variant::*;
mod live;
use live::*;
mod live2;
use live2::*;
mod jobs;
use jobs::*;
mod repl_ui;
use repl_ui::*;
mod keyboard_enhancement;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests2;
#[cfg(test)]
mod tests3;
