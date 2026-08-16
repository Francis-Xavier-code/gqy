//! ipc — 自 src/cli.rs 拆分。

pub(crate) use super::*;

pub(crate) async fn send_ipc_command(paths: &GQYPaths, command: IpcCommand) -> Result<()> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(&mut stream, &IpcRequest::new(command)).await?;
    validate_ipc_command_response(ipc::receive::<IpcFrame>(&mut stream).await?)
}

pub(crate) fn validate_ipc_command_response(frame: Option<IpcFrame>) -> Result<()> {
    match frame {
        Some(IpcFrame::Ack) | Some(IpcFrame::Ready { .. }) | Some(IpcFrame::AdminResult { .. }) => {
            Ok(())
        }
        Some(IpcFrame::Error { message, .. }) => bail!("{message}"),
        Some(other) => bail!("GQY core returned an unexpected response: {other:?}"),
        None => bail!("GQY core closed the connection without a response"),
    }
}

/// Refreshes REPL-local state after the daemon switched to another session:
/// input history, queue tray, and the footer's token accounting.
/// Writes one line of REPL feedback through the live tail so the output
/// cursor stays in sync; never use bare `println!` inside the remote REPL.
pub(crate) fn repl_note(live: &mut LiveReplTail, text: &str) -> Result<()> {
    live.apply_output_frame(format!("{text}\n").as_bytes())
}

/// Remote-REPL text equivalent of `print_pop_outcome` (which stays
/// println-based for the direct REPL and one-shot `gqy pop`).
pub(crate) fn repl_pop_outcome_text(outcome: PopOutcome) -> String {
    let message = if is_zh() {
        if outcome.archived {
            format!("已弹出 {} 轮 · 已归档", outcome.turns)
        } else {
            format!(
                "已弹出 {} 轮 · 未归档（弹出上下文归档已关闭）",
                outcome.turns
            )
        }
    } else {
        let turns = if outcome.turns == 1 { "turn" } else { "turns" };
        if outcome.archived {
            format!("popped {} {turns} · archived", outcome.turns)
        } else {
            format!(
                "popped {} {turns} · not archived (evicted-context archiving is disabled)",
                outcome.turns
            )
        }
    };
    format!("\x1b[2m{message}\x1b[0m\n")
}

/// Remote-REPL text equivalent of `print_nothing_to_pop`.
pub(crate) fn repl_nothing_to_pop_text() -> String {
    format!(
        "\x1b[2m{}\x1b[0m\n",
        t(
            "no conversation turns are available to pop",
            "没有可弹出的上下文轮次"
        )
    )
}

/// Client-side display fallback for sessions the server has not named yet.
pub(crate) fn display_session_name(name: &str) -> &str {
    if name.trim().is_empty() {
        t("New session", "新会话")
    } else {
        name
    }
}

pub(crate) async fn apply_repl_session_switch(
    paths: &GQYPaths,
    config: &AppConfig,
    state: &ipc::SessionState,
    active_session_id: &mut String,
    history: &mut Vec<String>,
    live_repl: &mut LiveReplTail,
    footer: &mut ReplFooterStatus,
    cumulative_tokens: &mut TurnTokens,
) -> Result<()> {
    if state.session_id.is_empty() {
        bail!("{}", t("session state has no id", "会话状态缺少 ID"));
    }
    let store = StateStore::new(paths)?.pinned(&state.session_id);
    active_session_id.clone_from(&state.session_id);
    *history = load_repl_input_history(&store, paths)?;
    live_repl.editor.history = history.clone();
    live_repl.editor.history_index = live_repl.editor.history.len();
    live_repl.editor.history_clean_index = None;
    live_repl.editor.input.clear();
    live_repl.editor.cursor = 0;
    repl_note(
        live_repl,
        &format!(
            "\x1b[2m{}: {}\x1b[0m\n",
            t("switched to session", "已切换到会话"),
            display_session_name(&state.session_name)
        ),
    )?;
    synchronized_terminal_update(CursorAfterUpdate::Shown, || live_repl.reload_queue(&store))?;
    // Rebuild rather than reset: the target session may pin its own model
    // pool, so provider/model/thinking have to be re-derived alongside the
    // token numbers. `refresh_footer` repaints straight away — merely storing
    // the footer left the previous session's numbers on screen until the next
    // turn finished.
    *cumulative_tokens = state_cumulative(&state);
    let session_config = footer_config_for_session(paths, config, &state.session_id);
    *footer =
        ReplFooterStatus::from_config(&session_config, state.context_tokens, *cumulative_tokens);
    let client = OpenAiCompatibleClient::from_config(&session_config, paths)?;
    footer.update_thinking_variant(client.thinking_variant_summary().as_deref());
    footer.update_context_window(state.context_window);
    live_repl.refresh_footer(footer.clone())?;
    // Every REPL session change funnels through here, so this is the one place
    // the REPL lane needs to be remembered. Best effort: losing the write only
    // means the next REPL starts on the terminal session.
    let _ = send_ipc_admin(
        paths,
        IpcCommand::SetReplSession {
            target: crate::ipc::SessionRef::Id {
                id: state.session_id.clone(),
            },
        },
    )
    .await;
    Ok(())
}

/// One row of the daemon's session list, parsed from `ListSessions` JSON.
pub(crate) struct SessionListEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) is_current: bool,
    pub(crate) turns: u64,
    pub(crate) snippet: String,
    pub(crate) workspace: Option<String>,
    /// "dev" | "normal",由 daemon 按会话人格推导。
    pub(crate) mode: String,
}

pub(crate) fn session_list_entries(data: &serde_json::Value) -> Vec<SessionListEntry> {
    data.get("sessions")
        .and_then(serde_json::Value::as_array)
        .map(|sessions| sessions.iter().map(session_list_entry).collect())
        .unwrap_or_default()
}

pub(crate) fn session_list_entry(session: &serde_json::Value) -> SessionListEntry {
    let text = |key: &str| {
        session
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    SessionListEntry {
        id: text("session_id").unwrap_or_default(),
        name: text("name").unwrap_or_default(),
        is_current: session
            .get("is_current")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        turns: session
            .get("turn_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        snippet: text("last_user_content")
            .map(|content| {
                let cleaned = content.trim().replace(['\n', '\r'], " ");
                let truncated: String = cleaned.chars().take(24).collect();
                if cleaned.chars().count() > 24 {
                    format!("{truncated}…")
                } else {
                    truncated
                }
            })
            .unwrap_or_default(),
        workspace: text("workspace"),
        mode: text("mode").unwrap_or_else(|| "normal".to_string()),
    }
}

/// Maps a user-facing 1-based session number to a session id ref.
pub(crate) fn session_ref_from_index(
    entries: &[SessionListEntry],
    index: usize,
) -> Option<crate::ipc::SessionRef> {
    index
        .checked_sub(1)
        .and_then(|index| entries.get(index))
        .map(|entry| crate::ipc::SessionRef::Id {
            id: entry.id.clone(),
        })
}

pub(crate) fn session_entry_is_active(
    entry: &SessionListEntry,
    active_session_id: Option<&str>,
) -> bool {
    active_session_id.map_or(entry.is_current, |session_id| entry.id == session_id)
}

pub(crate) fn session_select_line(
    entry: &SessionListEntry,
    active_session_id: Option<&str>,
) -> String {
    let marker = if session_entry_is_active(entry, active_session_id) {
        "* "
    } else {
        "  "
    };
    // 验收三轮定版:「模式：名称 · 摘要」,轮数删掉。
    let mut line = format!(
        "{marker}{}：{}",
        session_mode_label(&entry.mode),
        display_session_name(&entry.name),
    );
    if !entry.snippet.is_empty() {
        line.push_str(" · ");
        line.push_str(&entry.snippet);
    }
    if let Some(workspace) = &entry.workspace {
        line.push_str(&format!("  [{workspace}]"));
    }
    line
}

pub(crate) fn session_select_search(entry: &SessionListEntry) -> String {
    format!(
        "{} {} {} {}",
        display_session_name(&entry.name),
        session_mode_label(&entry.mode),
        entry.snippet,
        entry.workspace.as_deref().unwrap_or_default()
    )
}

/// 会话类型标(验收:列表看不出普通/开发)。
pub(crate) fn session_mode_label(mode: &str) -> &'static str {
    if mode == "dev" {
        t("dev", "开发")
    } else {
        t("normal", "普通")
    }
}

pub(crate) fn session_initial_selection(
    entries: &[SessionListEntry],
    active_session_id: Option<&str>,
) -> usize {
    entries
        .iter()
        .position(|entry| session_entry_is_active(entry, active_session_id))
        .unwrap_or(0)
}

/// What the interactive session picker came back with.
pub(crate) enum SessionPick {
    Cancelled,
    Switch(crate::ipc::SessionRef),
    /// Deletion confirmed inside the picker. `index` is where the cursor sat,
    /// so the caller can reopen the refreshed list at the same spot.
    Delete {
        session_id: String,
        index: usize,
    },
}

pub(crate) fn select_session_target(
    entries: &[SessionListEntry],
    active_session_id: Option<&str>,
    cursor: Option<usize>,
) -> Result<SessionPick> {
    let lines = entries
        .iter()
        .map(|entry| session_select_line(entry, active_session_id))
        .collect::<Vec<_>>();
    let search = entries
        .iter()
        .map(session_select_search)
        .collect::<Vec<_>>();
    let labels = entries
        .iter()
        .map(|entry| display_session_name(&entry.name).to_string())
        .collect::<Vec<_>>();
    let initial = cursor
        .map(|index| index.min(entries.len().saturating_sub(1)))
        .unwrap_or_else(|| session_initial_selection(entries, active_session_id));
    Ok(
        match inline_single_select_deletable(
            t("Select session", "选择会话"),
            &lines,
            &search,
            initial,
            Some(&labels),
        )? {
            InlineSelectOutcome::Cancelled => SessionPick::Cancelled,
            InlineSelectOutcome::Chosen(index) => SessionPick::Switch(crate::ipc::SessionRef::Id {
                id: entries[index].id.clone(),
            }),
            InlineSelectOutcome::Deleted(index) => SessionPick::Delete {
                session_id: entries[index].id.clone(),
                index,
            },
        },
    )
}

/// Resolves a user-typed `/session` / `/delete` argument into a session ref:
/// a number picks from the visible session list, anything else is a name.
/// REPL 会话列表的作用域:dev REPL 只看/只解析 dev 人格名下的会话。
pub(crate) fn repl_list_mode(mode: AgentMode) -> Option<String> {
    (mode == AgentMode::Dev).then(|| "dev".to_string())
}

pub(crate) async fn resolve_repl_session_target(
    paths: &GQYPaths,
    live: &mut LiveReplTail,
    mode: AgentMode,
    arg: &str,
) -> Result<Option<crate::ipc::SessionRef>> {
    let index = arg.parse::<usize>().ok();
    // 名字寻址在 daemon 侧按"当前人格"检索,够不着 dev 会话;dev REPL
    // 统一走列表在客户端配对,再降成不可猜的 id 显式寻址。
    if index.is_some() || mode == AgentMode::Dev {
        let Some((_, data)) = repl_ipc_admin(
            paths,
            live,
            IpcCommand::ListSessions {
                mode: repl_list_mode(mode),
            },
        )
        .await?
        else {
            return Ok(None);
        };
        let entries = session_list_entries(&data);
        let target = match index {
            Some(index) => session_ref_from_index(&entries, index),
            None => entries.iter().find(|entry| entry.name == arg).map(|entry| {
                crate::ipc::SessionRef::Id {
                    id: entry.id.clone(),
                }
            }),
        };
        let Some(target) = target else {
            repl_note(
                live,
                &format!(
                    "\x1b[2m{}: {arg}\x1b[0m\n",
                    t("no such session", "没有这个会话")
                ),
            )?;
            return Ok(None);
        };
        Ok(Some(target))
    } else {
        Ok(Some(crate::ipc::SessionRef::Name {
            name: arg.to_string(),
        }))
    }
}

pub(crate) fn reload_repl_queue(
    live: &mut LiveReplTail,
    paths: &GQYPaths,
    session_id: &str,
) -> Result<()> {
    let store = StateStore::new(paths)?.pinned(session_id);
    synchronized_terminal_update(CursorAfterUpdate::Shown, || live.reload_queue(&store))
}

pub(crate) fn confirm_inline(live: &mut LiveReplTail, prompt: &str) -> Result<bool> {
    live.apply_output_frame(format!("{prompt} [y/N] ").as_bytes())?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

pub(crate) fn confirm_stdin(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

/// Sends an admin command from inside the REPL loop, printing failures (core
/// busy, core restarting, …) through the live tail instead of propagating
/// them so the REPL survives.
pub(crate) async fn repl_ipc_admin(
    paths: &GQYPaths,
    live: &mut LiveReplTail,
    command: IpcCommand,
) -> Result<Option<(ipc::SessionState, serde_json::Value)>> {
    match send_ipc_admin(paths, command).await {
        Ok(result) => Ok(Some(result)),
        Err(err) => {
            repl_note(
                live,
                &format!("\x1b[31m{}: {err}\x1b[0m\n", t("error", "错误")),
            )?;
            Ok(None)
        }
    }
}

pub(crate) async fn repl_get_session_state(
    paths: &GQYPaths,
    live: &mut LiveReplTail,
    target: crate::ipc::SessionRef,
) -> Result<Option<ipc::SessionState>> {
    Ok(
        repl_ipc_admin(paths, live, IpcCommand::GetSessionState { target })
            .await?
            .map(|(state, _)| state),
    )
}

pub(crate) async fn repl_fallback_session_state(
    paths: &GQYPaths,
    live: &mut LiveReplTail,
    mode: AgentMode,
) -> Result<Option<ipc::SessionState>> {
    // dev 无普通人格的"终端会话"可退:GetReplSession 会治愈指针并在
    // 没有 dev 会话时就地自举一个,绝不落回普通人格的会话。
    if mode == AgentMode::Dev {
        return Ok(repl_ipc_admin(
            paths,
            live,
            IpcCommand::GetReplSession {
                mode: Some("dev".to_string()),
            },
        )
        .await?
        .map(|(state, _)| state));
    }
    let Some((_, data)) =
        repl_ipc_admin(paths, live, IpcCommand::ListSessions { mode: None }).await?
    else {
        return Ok(None);
    };
    let entries = session_list_entries(&data);
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.is_current)
        .or_else(|| entries.first())
    else {
        return Ok(None);
    };
    repl_get_session_state(
        paths,
        live,
        crate::ipc::SessionRef::Id {
            id: entry.id.clone(),
        },
    )
    .await
}

/// Runs the interactive session picker inside the REPL, servicing Ctrl+D
/// deletions in place. Returns the session state to switch to — a fallback
/// session when the REPL's own session was one of the ones deleted, so backing
/// out never strands the REPL on a session that no longer exists.
pub(crate) async fn repl_pick_session(
    paths: &GQYPaths,
    live: &mut LiveReplTail,
    mode: AgentMode,
    active_session_id: &str,
) -> Result<Option<ipc::SessionState>> {
    let mut cursor = None;
    let mut lost_active = false;
    loop {
        let Some((_, data)) = repl_ipc_admin(
            paths,
            live,
            IpcCommand::ListSessions {
                mode: repl_list_mode(mode),
            },
        )
        .await?
        else {
            return Ok(None);
        };
        let entries = session_list_entries(&data);
        if entries.is_empty() {
            repl_note(
                live,
                &format!("\x1b[2m{}\x1b[0m\n", t("no sessions", "没有会话")),
            )?;
            // Deleting the last session leaves the daemon to mint a fresh one.
            return if lost_active {
                repl_fallback_session_state(paths, live, mode).await
            } else {
                Ok(None)
            };
        }
        match select_session_target(&entries, Some(active_session_id), cursor)? {
            SessionPick::Cancelled => {
                return if lost_active {
                    repl_fallback_session_state(paths, live, mode).await
                } else {
                    Ok(None)
                };
            }
            SessionPick::Switch(target) => {
                return repl_get_session_state(paths, live, target).await;
            }
            SessionPick::Delete { session_id, index } => {
                let was_active = session_id == active_session_id;
                let deleted = repl_ipc_admin(
                    paths,
                    live,
                    IpcCommand::DeleteSession {
                        target: crate::ipc::SessionRef::Id { id: session_id },
                    },
                )
                .await?;
                if deleted.is_none() {
                    return if lost_active {
                        repl_fallback_session_state(paths, live, mode).await
                    } else {
                        Ok(None)
                    };
                }
                lost_active |= was_active;
                // The rows below shift up, so holding the index parks the
                // cursor on the next session instead of jumping to the top.
                cursor = Some(index);
            }
        }
    }
}

pub(crate) async fn repl_active_or_default_state(
    paths: &GQYPaths,
    active_session_id: &str,
) -> Result<(ipc::SessionState, bool)> {
    match send_ipc_admin(
        paths,
        IpcCommand::GetSessionState {
            target: crate::ipc::SessionRef::Id {
                id: active_session_id.to_string(),
            },
        },
    )
    .await
    {
        Ok((state, _)) => Ok((state, false)),
        Err(_) => {
            let (state, _) = send_ipc_admin(paths, IpcCommand::GetStatus).await?;
            let changed = state.session_id != active_session_id;
            Ok((state, changed))
        }
    }
}

/// Ensures the daemon is running, then sends one admin command; used by the
/// one-shot session subcommands (`gqy new/session/rename/...`).
pub(crate) async fn session_admin(
    paths: &GQYPaths,
    command: IpcCommand,
) -> Result<(ipc::SessionState, serde_json::Value)> {
    ipc::ensure_daemon(paths, None).await?;
    let refreshed = GQYPaths::new()?;
    send_ipc_admin(&refreshed, command).await
}

/// Resolves a `gqy session/delete` target argument outside the REPL:
/// numbers index into the visible session list, anything else is a name.
/// Resolves a `--session` argument (name or list index) to a concrete
/// session id, without moving the global current pointer.
pub(crate) async fn resolve_session_id_for_turn(paths: &GQYPaths, arg: &str) -> Result<String> {
    let (_, data) = session_admin(paths, IpcCommand::ListSessions { mode: None }).await?;
    let entries = session_list_entries(&data);
    if let Ok(index) = arg.parse::<usize>() {
        if let Some(entry) = index.checked_sub(1).and_then(|index| entries.get(index)) {
            return Ok(entry.id.clone());
        }
        bail!(
            "{}: {index}",
            t("no session with this number", "没有这个编号的会话")
        );
    }
    entries
        .into_iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(arg) || entry.id == arg)
        .map(|entry| entry.id)
        .ok_or_else(|| anyhow::anyhow!("{}: {arg}", t("session not found", "找不到该会话")))
}

/// Expands a leading `~` or `~/…` to the user's home directory.
pub(crate) fn expand_tilde(path: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        if path == "~" {
            return PathBuf::from(home);
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

pub(crate) async fn send_ipc_admin(
    paths: &GQYPaths,
    command: IpcCommand,
) -> Result<(ipc::SessionState, serde_json::Value)> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(&mut stream, &IpcRequest::new(command)).await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::AdminResult { state, data }) => Ok((state, data)),
        Some(IpcFrame::Error { message, .. }) => bail!("{message}"),
        _ => bail!("GQY core returned an invalid admin response"),
    }
}

pub(crate) fn ipc_text<'a>(data: &'a serde_json::Value, key: &str) -> &'a str {
    data.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

pub(crate) async fn render_remote_tool_image(
    state: &StateStore,
    event: &serde_json::Value,
    size: Option<String>,
) -> Result<()> {
    let asset_id = remote_tool_image_asset_id(event)
        .ok_or_else(|| anyhow::anyhow!("tool image event did not contain an asset id"))?;
    let asset = state
        .load_image_asset(asset_id)?
        .ok_or_else(|| anyhow::anyhow!("tool image asset is no longer available"))?;
    let preview = remote_image_preview(&asset)?;
    tools::vision::print_image_file(preview.path(), size).await
}

pub(crate) fn remote_tool_image_asset_id(event: &serde_json::Value) -> Option<&str> {
    event
        .get("asset")
        .and_then(|asset| asset.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

pub(crate) fn remote_image_preview(
    asset: &crate::state::ImageAssetData,
) -> Result<tempfile::NamedTempFile> {
    let suffix = if asset.asset.mime == "image/gif" {
        ".png"
    } else {
        match asset.asset.mime.as_str() {
            "image/jpeg" => ".jpg",
            "image/png" => ".png",
            "image/webp" => ".webp",
            "image/bmp" => ".bmp",
            _ => ".img",
        }
    };
    let mut preview = tempfile::Builder::new().suffix(suffix).tempfile()?;
    if asset.asset.mime == "image/gif" {
        image::load_from_memory(&asset.bytes)?
            .write_to(preview.as_file_mut(), image::ImageFormat::Png)?;
    } else {
        preview.write_all(&asset.bytes)?;
        preview.flush()?;
    }
    Ok(preview)
}

pub(crate) fn ipc_mode_name(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Normal => "normal",
        AgentMode::Dev => "dev",
    }
}

pub(crate) fn ipc_images(
    images: &[Option<crate::clipboard::PastedImage>],
) -> Vec<Option<crate::ipc::ImageAttachment>> {
    images
        .iter()
        .map(|image| {
            image.as_ref().map(|image| match image {
                crate::clipboard::PastedImage::Binary(image) => {
                    crate::ipc::ImageAttachment::Binary {
                        mime: image.mime.clone(),
                        data: image.data.clone(),
                    }
                }
                crate::clipboard::PastedImage::Path(path) => {
                    crate::ipc::ImageAttachment::Path { path: path.clone() }
                }
            })
        })
        .collect()
}

pub(crate) fn print_chat_token_usage(
    result: &crate::llm::ChatResult,
    enabled: bool,
    session_token_total: u64,
    context_window: Option<usize>,
    cumulative: TurnTokens,
) -> Result<()> {
    if enabled && result.usage.is_some() {
        let meter = turn_meter(
            TurnTokens::from_usage(result.usage.as_ref()),
            session_token_total,
            context_window,
            cumulative,
        );
        render::print_token_usage(&meter, result.usage_estimated)?;
    }
    Ok(())
}

pub(crate) fn turn_meter(
    turn: TurnTokens,
    session_tokens: u64,
    context_window: Option<usize>,
    cumulative: TurnTokens,
) -> render::TokenMeter {
    render::TokenMeter {
        turn_tokens: turn.total,
        turn_prompt_tokens: turn.prompt,
        turn_cached_tokens: turn.cache_read,
        session_tokens,
        context_window,
        ..meter_cumulative(cumulative)
    }
}

pub(crate) fn result_context_window(
    config: &AppConfig,
    result: &crate::llm::ChatResult,
) -> Option<usize> {
    if config.active_provider_model_choices().len() > 1 {
        return None;
    }
    let provider = result.provider_id.as_deref()?;
    let model = result.model.as_deref()?;
    config
        .context_window_for_provider_model(provider, model)
        .ok()
        .flatten()
}

pub(crate) async fn handle_post_turn_overflow(
    agent: &Agent,
    renderer: &mut render::StreamRenderer,
    context_tokens: u64,
    show_token_usage: bool,
    cumulative_tokens: Option<&mut TurnTokens>,
) -> Result<Option<crate::llm::ChatResult>> {
    let compact_result = agent
        .handle_overflow_after_turn(context_tokens, |event| handle_agent_event(renderer, event))
        .await?;
    renderer.finish()?;
    if let Some(compact_result) = compact_result {
        let mut cumulative_display = TurnTokens::default();
        if let Some(total) = cumulative_tokens {
            if let Some(usage) = compact_result.usage.as_ref() {
                total.add(TurnTokens::from_usage(Some(usage)));
                cumulative_display = *total;
            }
        }
        print_chat_token_usage(
            &compact_result,
            show_token_usage,
            agent.effective_context_tokens()?,
            agent.context_window(),
            cumulative_display,
        )?;
        return Ok(Some(compact_result));
    }
    Ok(None)
}

pub(crate) async fn handle_live_post_turn_overflow(
    live: &mut LiveReplTail,
    agent: &Agent,
    renderer: &mut render::StreamRenderer,
    context_tokens: u64,
    show_token_usage: bool,
    cumulative_tokens: Option<&mut TurnTokens>,
) -> Result<Option<crate::llm::ChatResult>> {
    let compact_result = agent
        .handle_overflow_after_turn(context_tokens, |event| {
            handle_live_agent_event(live, renderer, event)
        })
        .await?;
    renderer.finish()?;
    live.apply_renderer_frame(renderer)?;
    if let Some(compact_result) = compact_result {
        let mut cumulative_display = TurnTokens::default();
        if let Some(total) = cumulative_tokens {
            if let Some(usage) = compact_result.usage.as_ref() {
                total.add(TurnTokens::from_usage(Some(usage)));
                cumulative_display = *total;
            }
        }
        if show_token_usage {
            if let Some(usage) = compact_result.usage.as_ref() {
                let frame = render::token_usage_output(
                    &turn_meter(
                        TurnTokens::from_usage(Some(usage)),
                        agent.effective_context_tokens()?,
                        agent.context_window(),
                        cumulative_display,
                    ),
                    compact_result.usage_estimated,
                );
                live.apply_output_frame(frame.strip_suffix('\n').unwrap_or(&frame).as_bytes())?;
            }
        }
        return Ok(Some(compact_result));
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VariantOutcome {
    Updated,
    Cancelled,
    Rejected(String),
}

pub(crate) fn run_variant(paths: &GQYPaths, args: VariantArgs) -> Result<()> {
    let selected = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if selected.is_none() && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        bail!(
            "{}",
            t(
                "interactive variant selection requires a terminal; use `gqy variant <name>`",
                "交互 variant 选择需要终端；请使用 `gqy variant <名称>`",
            )
        );
    }
    if !crate::models_cache::is_loaded() {
        crate::models_cache::refresh_blocking(paths).map_err(|error| {
            anyhow::anyhow!(
                "{}: {error:#}",
                t("failed to load model metadata", "无法加载模型元数据")
            )
        })?;
    }

    let config = AppConfig::load_or_default(paths)?;
    let mut client = OpenAiCompatibleClient::from_config(&config, paths)?;
    match execute_variant(paths, &mut client, selected, "gqy variant")? {
        VariantOutcome::Updated => print_variant_updated(),
        VariantOutcome::Cancelled => {}
        VariantOutcome::Rejected(message) => bail!("{message}"),
    }
    Ok(())
}

pub(crate) fn execute_variant(
    paths: &GQYPaths,
    client: &mut OpenAiCompatibleClient,
    selected: Option<&str>,
    selector_command: &str,
) -> Result<VariantOutcome> {
    if let Some(selected) = selected {
        if client.thinking_variant_options().len() != 1 {
            let message = if is_zh() {
                format!("当前激活了多个模型；请使用 {selector_command} 在 TUI 中分别设置")
            } else {
                format!(
                    "multiple models are active; use {selector_command} and configure them in the TUI"
                )
            };
            return Ok(VariantOutcome::Rejected(message));
        }
        let available = client.available_thinking_variants();
        let variant = match resolve_variant_name(selected, &available) {
            Ok(variant) => variant,
            Err(message) => return Ok(VariantOutcome::Rejected(message)),
        };
        client.set_thinking_variant(variant)?;
    } else {
        let options = client.thinking_variant_options();
        let Some(selections) = inline_variant_select(&options)? else {
            return Ok(VariantOutcome::Cancelled);
        };
        client.set_thinking_variants(&selections)?;
    }

    client.save_thinking_variants(paths)?;
    Ok(VariantOutcome::Updated)
}

pub(crate) fn resolve_variant_name(
    selected: &str,
    available: &[String],
) -> std::result::Result<Option<String>, String> {
    let explicit_variant = selected.strip_prefix("variant:");
    if explicit_variant.is_none() && selected.eq_ignore_ascii_case("default") {
        return Ok(None);
    }
    let selected = explicit_variant.unwrap_or(selected);
    available
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(selected))
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            format!(
                "{}: {selected}",
                t("unknown thinking variant", "未知思考档位")
            )
        })
}

pub(crate) fn print_variant_updated() {
    println!("{}\n", t("thinking variants updated", "已更新思考档位"));
}

pub(crate) fn print_mode_help() {
    if crate::i18n::is_zh() {
        println!("请选择模式。想让裸 gqy 命令直接进某个模式,可以在设置中修改(config.jsonc 的 default_mode)。\n");
        println!("  gqy normal   普通模式。可使用全部工具,适合日常使用。支持角色扮演、娱乐聊天、记忆、技能等全部能力。");
        println!("  gqy dev      开发模式。与普通模式明确区分,用于开发工作;移除与开发无关的角色扮演与娱乐工具,提示词极简可编辑,记忆独立。");
        println!("  gqy '<your_prompts>'   使用普通模式进行一次性对话");
    } else {
        println!("Pick a mode. To make bare `gqy` enter one directly, set default_mode in config.jsonc.\n");
        println!("  gqy normal   full-capability mode: persona, memory, every tool.");
        println!("  gqy dev      development mode: minimal editable prompt, coding tools only, separate memory.");
        println!("  gqy '<your_prompts>'   one-shot ask in normal mode");
    }
}

pub(crate) async fn run_repl(paths: &GQYPaths, initial_mode: AgentMode) -> Result<()> {
    if direct_mode_requested() {
        run_direct_repl(paths, initial_mode).await
    } else {
        run_remote_repl(paths, initial_mode).await
    }
}
