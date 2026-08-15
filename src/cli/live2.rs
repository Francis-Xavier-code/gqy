//! live2 — 自 src/cli/live.rs 拆分。

use super::*;

pub(crate) fn repl_input_rendered_rows(
    input: &str,
    is_pasted: bool,
    show_shortcut_hint: bool,
    cols: usize,
) -> u16 {
    let suggestions = repl_command_suggestions(input);
    let lines = repl_input_lines(input);
    let display_lines =
        repl_visible_input_lines("  ", &lines, REPL_MAX_VISIBLE_INPUT_ROWS, is_pasted);
    let input_rows = repl_wrapped_input_rows_for_cols("  ", &display_lines, cols)
        .len()
        .max(1)
        .min(u16::MAX as usize) as u16;
    input_rows.saturating_add(if show_shortcut_hint && suggestions.is_empty() {
        4
    } else {
        3
    })
}

pub(crate) const JOB_SPINNER_FRAMES: [char; 10] =
    ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub(crate) fn format_job_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// Status strip under the footer: a leading blank line, then one line per
/// background command with a blank line between entries. Timers are
/// right-aligned to the terminal width.
pub(crate) fn background_job_lines(
    jobs: &[crate::tools::jobs::JobOverview],
    spinner_phase: usize,
    cols: usize,
) -> Vec<String> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let kind_label = |job: &crate::tools::jobs::JobOverview| {
        if job.kind == "subagent" {
            crate::i18n::text("agent", "子代理")
        } else {
            crate::i18n::text("cmd", "命令")
        }
    };
    // Pad kinds to one column so mixed command/subagent rows keep their ids
    // and titles vertically aligned.
    let kind_col = jobs
        .iter()
        .map(|job| visible_width(kind_label(job)))
        .max()
        .unwrap_or(0);
    let mut lines = vec![String::new()];
    for job in jobs.iter() {
        let marker = JOB_SPINNER_FRAMES[spinner_phase % JOB_SPINNER_FRAMES.len()];
        let kind_word = kind_label(job);
        let kind_pad = " ".repeat(kind_col.saturating_sub(visible_width(kind_word)));
        let mut left = format!(
            "{marker} {kind_word}{kind_pad} {} · {}",
            job.job_id, job.title
        );
        let timer = format_job_duration(job.runtime_seconds);
        let timer_width = visible_width(&timer);
        // Never exceed the terminal width: a wrapped strip line would shift
        // the whole tail and flicker.
        let max_left = cols.saturating_sub(timer_width).saturating_sub(2);
        while visible_width(&left) > max_left && !left.is_empty() {
            left.pop();
        }
        let left_width = visible_width(&left);
        let pad = cols
            .saturating_sub(left_width)
            .saturating_sub(timer_width)
            .max(1);
        lines.push(format!("\x1b[2m{left}{}{timer}\x1b[0m", " ".repeat(pad)));
    }
    lines
}

pub(crate) fn queued_prompt_lines(
    prompts: &[QueuedPrompt],
    mode: AgentMode,
    cols: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    for (index, prompt) in prompts.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.extend(submitted_echo_lines(mode, &prompt.display_content, cols));
        lines.push(format!(
            "{} {}",
            submitted_echo_bar(mode),
            primary_footer_text(t("Queued", "排队中"))
        ));
    }
    lines
}

pub(crate) fn write_committed_user_messages(
    messages: &[(&str, AgentMode)],
    leading_gap: bool,
) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let mut stdout = io::stdout();
    let col = cursor_col_or(0);
    if col > 0 {
        writeln!(stdout)?;
    }
    let cols = terminal_cols();
    write!(
        stdout,
        "{}",
        committed_user_messages_text(messages, leading_gap, cols)
    )?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn committed_user_messages_text(
    messages: &[(&str, AgentMode)],
    leading_gap: bool,
    cols: usize,
) -> String {
    let mut output = String::new();
    if leading_gap {
        output.push('\n');
    }
    for (index, (content, mode)) in messages.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        for line in submitted_echo_lines(*mode, content, cols) {
            output.push_str(&line);
            output.push('\n');
        }
    }
    output.push('\n');
    output
}

/// Strips the bracketed prefix off a background-job wake headline, leaving
/// `子代理完成 82bea3 · 标题`. The older `[后台命令完成] ` spelling still shows
/// up in sessions recorded before the rename.
pub(crate) fn job_wake_headline(headline: &str) -> &str {
    headline
        .strip_prefix("[后台任务完成] ")
        .or_else(|| headline.strip_prefix("[后台命令完成] "))
        .unwrap_or(headline)
}

/// Fires a desktop notification unless the REPL window has focus.
///
/// `focused` is `None` when there is no live tail — a one-shot `gqy ask` has
/// no window to be away from, so it stays quiet.
pub(crate) fn notify_if_unfocused(
    config: &AppConfig,
    focused: Option<bool>,
    title: &str,
    body: &str,
) {
    if !config.notifications.enabled || focused != Some(false) {
        return;
    }
    crate::notify::notify(title, &crate::notify::clip_body(body, 120));
}

/// Redraws finished turns of a session as one ANSI frame.
///
/// Feeds the stored transcript back through the same `StreamRenderer` a live
/// turn uses, so tool blocks and prose come out identical — and re-wrapped for
/// the terminal's *current* width, which a saved byte transcript could not do.
/// Turns older than the transcript column fall back to prompt + final reply.
pub(crate) fn session_replay_frame(
    replays: &[crate::state::TurnReplay],
    mode: AgentMode,
    config: &AppConfig,
    cols: usize,
) -> Result<Vec<u8>> {
    use crate::state::ReplayEntry;
    let mut frame = Vec::new();
    for replay in replays {
        if replay.is_job_wake {
            // A background job woke the session; live rendering shows a dim
            // `⚙` notice, never a user bubble. Mirror that.
            frame.extend_from_slice(
                format!(
                    "\n\x1b[2m⚙ {}\x1b[0m\n\n",
                    job_wake_headline(&replay.display_content)
                )
                .as_bytes(),
            );
        } else if !replay.display_content.trim().is_empty() {
            frame.extend_from_slice(
                committed_user_messages_text(&[(&replay.display_content, mode)], true, cols)
                    .as_bytes(),
            );
        }
        let mut renderer = render::StreamRenderer::new(
            render::ReasoningDisplayMode::Hidden,
            render::ToolCallDisplayMode::from_config(&config.display.tool_calls),
            false,
            config.display.readable_tool_names,
            config.display.command_output_lines,
        );
        renderer.use_external_cursor_control();
        renderer.use_buffered_output();
        if replay.entries.is_empty() {
            renderer.write_chunk(ChatStreamChunk {
                kind: crate::llm::ChatStreamKind::Content,
                text: replay.assistant_content.clone(),
            })?;
        } else {
            for entry in &replay.entries {
                match entry {
                    ReplayEntry::Text { text } => renderer.write_chunk(ChatStreamChunk {
                        kind: crate::llm::ChatStreamKind::Content,
                        text: text.clone(),
                    })?,
                    ReplayEntry::ToolCall { name, arguments } => {
                        renderer.write_tool_call(name, arguments)?
                    }
                    ReplayEntry::ToolResult { name, ok, output } => {
                        renderer.write_tool_result(name, *ok, output)?
                    }
                }
            }
        }
        renderer.finish()?;
        frame.extend_from_slice(&renderer.take_output_frame());
    }
    Ok(frame)
}

pub(crate) fn queued_prompt_attachments(
    images: &[Option<crate::clipboard::PastedImage>],
) -> Vec<QueuedPromptAttachment> {
    images
        .iter()
        .filter_map(|image| match image {
            Some(crate::clipboard::PastedImage::Binary(image)) => {
                Some(QueuedPromptAttachment::Binary {
                    mime: image.mime.clone(),
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&image.data),
                })
            }
            Some(crate::clipboard::PastedImage::Path(path)) => {
                Some(QueuedPromptAttachment::Path { path: path.clone() })
            }
            None => None,
        })
        .collect()
}

pub(crate) fn persist_queued_submission(
    state: &StateStore,
    submission: &LiveSubmission,
) -> Result<QueuedPrompt> {
    let prompt_id = format!(
        "queued_{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0),
        rand::random::<u16>()
    );
    state.enqueue_prompt(
        &prompt_id,
        &submission.content,
        &submission.display_content,
        &queued_prompt_attachments(&submission.images),
    )
}

/// Queues a submission for the turn currently running in the daemon, using
/// the cross-process queue target so the daemon consumes it mid-turn.
pub(crate) async fn persist_remote_queued_submission(
    paths: &GQYPaths,
    run_id: &str,
    turn_id: &str,
    submission: &LiveSubmission,
) -> Result<QueuedPrompt> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::QueueTurnUpdate {
            run_id: run_id.to_string(),
            turn_id: turn_id.to_string(),
            content: submission.content.clone(),
            display_content: submission.display_content.clone(),
            images: ipc_images(&submission.images),
            supersede: false,
        }),
    )
    .await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::TurnUpdateAccepted {
            prompt_id,
            seq,
            submitted_at,
            ..
        }) => Ok(QueuedPrompt {
            prompt_id,
            seq,
            content: submission.content.clone(),
            display_content: submission.display_content.clone(),
            attachments: queued_prompt_attachments(&submission.images),
            uploaded_attachments: Vec::new(),
            submitted_at,
        }),
        Some(IpcFrame::Error { message, .. }) => bail!("{message}"),
        Some(_) => bail!("GQY core returned an invalid queue response"),
        None => bail!("GQY core closed the queue connection"),
    }
}

pub(crate) struct LiveRawMode {
    show_cursor_on_drop: bool,
    restore_terminal_on_drop: bool,
    keyboard_enhancement: KeyboardEnhancementState,
}

pub(crate) struct ReplCursorRestore;

impl Drop for ReplCursorRestore {
    pub(crate) fn drop(&mut self) {
        // 1. 会话级兜底：恢复括号粘贴与光标
        // 2. 再关闭 raw mode；键盘增强由 LiveRawMode / 局部输入作用域负责 Pop
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableFocusChange,
            Show
        );
        let _ = terminal::disable_raw_mode();
    }
}

impl LiveRawMode {
    /// 进入 live REPL 的 raw 输入模式，并尽量启用键盘增强协议。
    ///
    /// 参数: 无
    ///
    /// 返回:
    /// - 成功时返回会在 Drop 时恢复终端的守卫对象
    pub(crate) fn start() -> Result<Self> {
        enable_live_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnableBracketedPaste) {
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        // Focus reporting is advisory: terminals that ignore it simply never
        // send the events, and the editor stays on its "focused" default.
        let _ = execute!(stdout, EnableFocusChange);
        Ok(Self {
            show_cursor_on_drop: true,
            restore_terminal_on_drop: true,
            keyboard_enhancement: KeyboardEnhancementState::enable(&mut stdout),
        })
    }

    /// 接管上一段 live 输入已启用的终端模式，避免重复 Push 键盘增强。
    ///
    /// 参数: 无
    ///
    /// 返回:
    /// - 会在最终 Drop 时恢复终端的守卫对象
    pub(crate) fn adopt() -> Self {
        Self {
            show_cursor_on_drop: true,
            restore_terminal_on_drop: true,
            keyboard_enhancement: KeyboardEnhancementState::assume_active(),
        }
    }

    pub(crate) fn keep_cursor_hidden(&mut self) {
        self.show_cursor_on_drop = false;
    }

    pub(crate) fn handoff(&mut self) {
        self.restore_terminal_on_drop = false;
        // handoff 后由下一段 LiveRawMode::adopt 继续持有键盘增强状态
        self.keyboard_enhancement = KeyboardEnhancementState::default();
    }
}

pub(crate) fn enable_live_raw_mode() -> Result<()> {
    terminal::enable_raw_mode()?;
    spawn_hangup_watchdog();
    if let Err(error) = restore_live_output_processing() {
        let _ = terminal::disable_raw_mode();
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restore_live_output_processing() -> Result<()> {
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    // Raw input is required for key events, but renderer output still relies on newline translation.
    unsafe {
        if libc::tcgetattr(libc::STDOUT_FILENO, attributes.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut attributes = attributes.assume_init();
        attributes.c_oflag |= libc::OPOST | libc::ONLCR;
        if libc::tcsetattr(libc::STDOUT_FILENO, libc::TCSANOW, &attributes) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restore_live_output_processing() -> Result<()> {
    Ok(())
}

impl Drop for LiveRawMode {
    pub(crate) fn drop(&mut self) {
        if !self.restore_terminal_on_drop {
            return;
        }
        let mut stdout = io::stdout();
        if self.show_cursor_on_drop {
            let _ = execute!(stdout, DisableBracketedPaste, DisableFocusChange, Show);
        } else {
            let _ = execute!(stdout, DisableBracketedPaste, DisableFocusChange);
        }
        // 1. 先 Pop 键盘增强协议
        // 2. 再退出 raw mode
        self.keyboard_enhancement.disable(&mut stdout);
        let _ = terminal::disable_raw_mode();
    }
}

/// Shared feed state between the remote REPL and its IPC poll thread.
#[derive(Default)]
pub(crate) struct SharedJobsFeed {
    /// The owning REPL's current session — strip snapshots are filtered to
    /// it (daemon "current session" can drift from the REPL's after /new).
    repl_session: std::sync::Mutex<Option<String>>,
    jobs: std::sync::Mutex<Vec<crate::tools::jobs::JobOverview>>,
    /// Rendered wake-turn reports waiting to be printed into the scrollback.
    reports: std::sync::Mutex<Vec<BackgroundReport>>,
    /// Latest session Σ read straight from the store. Background subagents
    /// bill to the session that launched them, but they finish long after the
    /// turn that spawned them published its totals — without this the footer
    /// sat on a stale Σ until the user happened to send another prompt.
    cumulative: std::sync::Mutex<Option<TurnTokens>>,
    /// Active daemon-initiated wake runs: (run_id, session_id, label).
    wake_runs: std::sync::Mutex<Vec<(String, String, String)>>,
    /// Wake runs already attached to (never re-follow), and turn ids that
    /// were rendered live (their DB report must not print again).
    followed_runs: std::sync::Mutex<std::collections::HashSet<String>>,
    rendered_turns: std::sync::Mutex<std::collections::HashSet<String>>,
}

#[derive(Clone)]
pub(crate) struct BackgroundReport {
    turn_id: String,
    headline: String,
    reply: String,
}

/// Session isolation for the strip: keep only `session`'s jobs (sessionless
/// jobs stay visible as a legacy fallback; `None` session shows everything).
pub(crate) fn retain_session_jobs(
    jobs: &mut Vec<crate::tools::jobs::JobOverview>,
    session: Option<&str>,
) {
    if let Some(session) = session {
        jobs.retain(|job| job.session_id.is_none() || job.session_id.as_deref() == Some(session));
    }
}

/// Source of background-command snapshots for the idle status strip.
pub(crate) enum JobsFeed {
    /// Remote REPL: snapshots pushed by the IPC poll thread.
    Shared(std::sync::Arc<SharedJobsFeed>),
    /// Direct REPL: read the in-process registry.
    Local,
}

impl JobsFeed {
    pub(crate) fn current(&self) -> Vec<crate::tools::jobs::JobOverview> {
        match self {
            JobsFeed::Shared(shared) => shared.jobs.lock().unwrap().clone(),
            JobsFeed::Local => crate::tools::jobs::overview(),
        }
    }

    /// The store's current Σ for the REPL's session, or `None` when this feed
    /// has no store behind it.
    pub(crate) fn cumulative(&self) -> Option<TurnTokens> {
        match self {
            JobsFeed::Shared(shared) => *shared.cumulative.lock().unwrap(),
            JobsFeed::Local => None,
        }
    }

    pub(crate) fn take_reports(&self) -> Vec<BackgroundReport> {
        match self {
            JobsFeed::Shared(shared) => {
                let mut reports = shared.reports.lock().unwrap();
                let rendered = shared.rendered_turns.lock().unwrap();
                let taken = reports
                    .drain(..)
                    .filter(|report| !rendered.contains(&report.turn_id))
                    .collect();
                taken
            }
            JobsFeed::Local => Vec::new(),
        }
    }

    /// Next wake run in `session` that has not been followed yet; marks it
    /// followed so the caller attaches exactly once.
    pub(crate) fn claim_wake_run(&self, session: &str) -> Option<(String, String)> {
        let JobsFeed::Shared(shared) = self else {
            return None;
        };
        let wake_runs = shared.wake_runs.lock().unwrap();
        let mut followed = shared.followed_runs.lock().unwrap();
        for (run_id, run_session, label) in wake_runs.iter() {
            if run_session == session && !followed.contains(run_id) {
                followed.insert(run_id.clone());
                return Some((run_id.clone(), label.clone()));
            }
        }
        None
    }
}

/// Poll the daemon for background commands while the remote REPL idles:
/// 1s when commands are live, 3s when quiet — a unix-socket roundtrip
/// costs microseconds either way.
pub(crate) fn spawn_jobs_poll_thread(paths: GQYPaths) -> std::sync::Arc<SharedJobsFeed> {
    let shared = std::sync::Arc::new(SharedJobsFeed::default());
    let feed = shared.clone();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        // Track per-session watermarks so wake replies print exactly once,
        // and never replay history from before this REPL started.
        let mut seen: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        // The store open can lose a race against daemon writes (SQLITE_BUSY);
        // retry every cycle instead of deciding at startup forever.
        let mut store: Option<StateStore> = None;
        loop {
            if store.is_none() {
                store = StateStore::new(&paths).ok();
            }
            let (jobs, session_id, wake_runs) = runtime
                .block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        fetch_jobs_overview(&paths),
                    )
                    .await
                    .unwrap_or_else(|_| Ok((Vec::new(), None, Vec::new())))
                })
                .unwrap_or_default();
            let mut jobs = jobs;
            let repl_session = { feed.repl_session.lock().unwrap().clone() };
            retain_session_jobs(&mut jobs, repl_session.as_deref());
            *feed.jobs.lock().unwrap() = jobs;
            *feed.wake_runs.lock().unwrap() = wake_runs;
            if let (Some(store), Some(session)) = (store.as_ref(), repl_session.as_deref()) {
                if let Ok(totals) = store.pinned(session).session_cumulative_token_totals() {
                    *feed.cumulative.lock().unwrap() = Some(totals);
                }
            }
            if let (Some(store), Some(session_id)) = (store.as_ref(), session_id) {
                let watermark = match seen.entry(session_id.clone()) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let latest = store.latest_turn_seq(&session_id).unwrap_or(0);
                        *entry.insert(latest)
                    }
                };
                if let Ok(rows) = store.background_report_replies_after(&session_id, watermark) {
                    for (seq, turn_id, display, reply) in rows {
                        seen.insert(session_id.clone(), seq);
                        if feed.rendered_turns.lock().unwrap().contains(&turn_id) {
                            continue;
                        }
                        feed.reports.lock().unwrap().push(BackgroundReport {
                            turn_id,
                            headline: display,
                            reply,
                        });
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    shared
}
