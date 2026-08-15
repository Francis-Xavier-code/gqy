//! schema — 自 src/config.rs 拆分。

pub(crate) use super::*;

pub(crate) fn validate_api_quota_accounts(
    provider: &str,
    config: &ApiQuotaProviderConfig,
) -> Result<()> {
    if !config.api_key.trim().is_empty() && !config.accounts.is_empty() {
        bail!("plugins.api_quota.{provider} legacy api_key could not be migrated");
    }
    if config.accounts.len() > 32 {
        bail!("plugins.api_quota.{provider} supports at most 32 accounts");
    }
    let mut names = HashSet::with_capacity(config.accounts.len());
    let mut ids = HashSet::with_capacity(config.accounts.len());
    for account in &config.accounts {
        let name = account.name.trim();
        if name.is_empty() {
            bail!("plugins.api_quota.{provider} account name cannot be empty");
        }
        if name.chars().count() > 64 {
            bail!("plugins.api_quota.{provider} account name exceeds 64 characters");
        }
        if !names.insert(name) {
            bail!("duplicate plugins.api_quota.{provider} account name: {name}");
        }
        let id = account.id.trim();
        if !id.is_empty() && !ids.insert(id) {
            bail!("duplicate plugins.api_quota.{provider} account id: {id}");
        }
    }
    Ok(())
}

pub(crate) fn default_timeout() -> u64 {
    60
}

pub(crate) fn default_vision_response_header_timeout() -> u64 {
    15
}

pub(crate) fn default_vision_stream_idle_timeout() -> u64 {
    20
}

pub(crate) fn default_vision_image_timeout() -> u64 {
    60
}

pub(crate) fn default_mcp_timeout() -> u64 {
    30
}

pub(crate) fn default_prompts_dir() -> String {
    "prompts".to_string()
}

pub(crate) fn default_identities_dir() -> String {
    "identities".to_string()
}

pub(crate) fn default_user_identity_file() -> String {
    "user-identity.md".to_string()
}

pub(crate) fn normalized_relative_path(value: &str) -> Option<PathBuf> {
    normalize_relative_path(Path::new(value.trim()))
}

pub(crate) fn normalize_relative_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(component) => normalized.push(component),
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (!normalized.as_os_str().is_empty()).then_some(normalized)
}

pub(crate) fn relative_path_equals(value: &str, expected: &str) -> bool {
    normalized_relative_path(value).as_deref() == Some(Path::new(expected))
}

pub(crate) fn migrated_resource_path(paths: &GQYPaths, value: &str) -> Option<PathBuf> {
    paths.migrated_resource_path(Path::new(value.trim()))
}

pub(crate) fn fallback_resource_file(
    paths: &GQYPaths,
    namespace: &str,
    file_name: &str,
) -> PathBuf {
    if paths.resources_use_config_dir() {
        paths.config_dir.join(file_name)
    } else {
        paths.resource_dir().join(namespace).join(file_name)
    }
}

pub(crate) fn migrated_fallback_file(
    paths: &GQYPaths,
    value: &str,
    namespace: &str,
    file_name: &str,
) -> Option<PathBuf> {
    let path = Path::new(value.trim());
    let matches_current = path == paths.config_dir.join(file_name);
    let matches_legacy = paths
        .legacy_config_dir()
        .is_some_and(|legacy| path == legacy.join(file_name));
    (path.is_absolute() && (matches_current || matches_legacy))
        .then(|| fallback_resource_file(paths, namespace, file_name))
}

pub(crate) fn config_relative_path(paths: &GQYPaths, value: &str) -> PathBuf {
    let path = PathBuf::from(value.trim());
    if path.is_absolute() {
        path
    } else {
        paths.config_dir.join(path)
    }
}

pub(crate) fn persona_scope_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return "default".to_string();
    }
    let normalized = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if normalized.is_empty() {
        format!("persona-{}", &blake3::hash(name.as_bytes()).to_hex()[..12])
    } else {
        normalized
    }
}

pub(crate) fn default_temperature() -> f32 {
    1.0
}

pub(crate) fn is_default_timeout(value: &u64) -> bool {
    *value == default_timeout()
}

pub(crate) fn is_default_temperature(value: &f32) -> bool {
    (*value - default_temperature()).abs() < f32::EPSILON
}

pub(crate) fn default_anthropic_max_tokens() -> u32 {
    4096
}

pub(crate) fn default_context_window() -> usize {
    168_000
}

pub(crate) fn is_default_anthropic_max_tokens(value: &u32) -> bool {
    *value == default_anthropic_max_tokens()
}

pub(crate) fn default_provider_protocol() -> String {
    "auto".to_string()
}

pub(crate) fn is_auto_protocol(value: &str) -> bool {
    value.trim().is_empty() || value == "auto"
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_tools_loading_mode() -> String {
    // v7 §八点七 stub mode: byte-constant tools array + on-demand contracts.
    // "hybrid" (grow the tools array on load) and "full" remain available.
    "stub".to_string()
}

pub(crate) fn default_subagent_concurrency() -> usize {
    4
}

pub(crate) fn default_tools_timeout_secs() -> u64 {
    180
}

pub(crate) fn default_command_deny() -> Vec<String> {
    [
        "rm -rf /",
        "rm -rf ~",
        "mkfs.",
        "dd if=/dev/zero of=/dev/",
        ":(){ :|:& };:",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub(crate) fn default_display_language() -> String {
    "auto".to_string()
}

pub(crate) fn default_reasoning_display() -> String {
    "summary".to_string()
}

pub(crate) fn default_tool_call_display() -> String {
    "summary".to_string()
}

pub(crate) fn default_command_output_lines() -> usize {
    10
}

pub(crate) fn default_repl_replay_turns() -> usize {
    3
}

pub(crate) fn default_mixed_model_endpoint_display() -> String {
    "interactive".to_string()
}

pub(crate) fn default_memory_association_facts() -> usize {
    5
}

pub(crate) fn default_memory_diary_batch_size() -> usize {
    14
}

pub(crate) fn default_memory_short_diary_retention_days() -> u64 {
    14
}

pub(crate) fn default_memory_diary_promotion_recalls() -> u64 {
    3
}

pub(crate) fn default_memory_organizer_timeout_seconds() -> u64 {
    120
}

pub(crate) fn default_memory_association_episodes() -> usize {
    3
}

pub(crate) fn default_memory_association_max_chars() -> usize {
    1800
}

pub(crate) fn default_memory_snippet_chars() -> usize {
    500
}

pub(crate) fn default_memory_forget_after_days() -> u64 {
    90
}

pub(crate) fn default_memory_half_life_days() -> f64 {
    7.0
}

pub(crate) fn default_memory_min_strength() -> f64 {
    0.15
}

pub(crate) fn default_memory_review_boost() -> f64 {
    0.35
}

pub(crate) fn default_memory_min_task_chars() -> usize {
    16
}

pub(crate) fn default_memory_min_method_chars() -> usize {
    120
}

pub(crate) fn default_print_image_width_percent() -> u8 {
    45
}

pub(crate) fn default_print_image_height_percent() -> u8 {
    35
}

pub(crate) fn default_memes_width_percent() -> u8 {
    35
}

pub(crate) fn default_memes_height_percent() -> u8 {
    25
}

pub(crate) fn default_memes_max_image_mb() -> u64 {
    10
}

pub(crate) fn default_memes_search_max_results() -> usize {
    1
}

pub(crate) fn default_memes_auto_send_probability() -> f32 {
    0.05
}

pub(crate) fn default_web_search_max_results() -> usize {
    2
}

pub(crate) fn default_web_images_max_results() -> usize {
    5
}

pub(crate) fn default_web_images_source_mode() -> String {
    "auto".to_string()
}

pub(crate) fn default_web_images_max_download_mb() -> f64 {
    4.0
}

pub(crate) fn default_web_images_preview_count() -> usize {
    1
}

pub(crate) fn default_web_images_timeout() -> u64 {
    20
}

pub(crate) fn default_deep_research_dir() -> String {
    default_gqy_home()
        .join("data/documents/deep-thinking")
        .display()
        .to_string()
}

pub(crate) fn default_deep_research_depth() -> String {
    "high".to_string()
}

pub(crate) fn default_deep_research_max_review_revisions() -> usize {
    0
}

pub(crate) fn default_deep_research_max_tool_steps() -> usize {
    0
}

pub(crate) fn default_deep_research_tool_timeout() -> u64 {
    90
}

pub(crate) fn default_subagent_max_tool_steps() -> usize {
    100
}

pub(crate) fn default_image_generation_provider_type() -> String {
    "openai".to_string()
}

pub(crate) fn default_openai_images_base_url() -> String {
    "https://api.openai.com".to_string()
}

pub(crate) fn default_image_generation_model() -> String {
    "gpt-image-1".to_string()
}

pub(crate) fn default_image_generation_aspect_ratio() -> String {
    "自动".to_string()
}

pub(crate) fn default_image_generation_resolution() -> String {
    "1K".to_string()
}

pub(crate) fn default_image_generation_output_dir() -> String {
    default_gqy_home()
        .join("data/pictures/generated-images")
        .display()
        .to_string()
}

pub(crate) fn default_gqy_home() -> PathBuf {
    std::env::var_os("GQY_HOME")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".gqy")))
        .unwrap_or_else(|| PathBuf::from("~/.gqy"))
}

/// Returns the old absolute directory when the value was rewritten, so the
/// caller can carry any files across; `None` when nothing matched.
pub(crate) fn remap_managed_output_dir(
    value: &mut String,
    legacy_roots: &[PathBuf],
    destination_root: &Path,
    home: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let trimmed = value.trim();
    let expanded = trimmed
        .strip_prefix("~/")
        .map(|relative| home.join(relative))
        .unwrap_or_else(|| PathBuf::from(trimmed));
    for legacy_root in legacy_roots {
        let Ok(relative) = expanded.strip_prefix(legacy_root) else {
            continue;
        };
        let destination = destination_root.join(relative);
        *value = destination.display().to_string();
        return Some((expanded, destination));
    }
    None
}

/// Carries files left behind at a remapped output directory over to the new
/// one. Best effort: a file that cannot be moved is left where it is rather
/// than failing a config load over it.
pub(crate) fn relocate_managed_output(from: &Path, to: &Path) {
    if from == to || !from.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    let mut moved = 0usize;
    for entry in entries.flatten() {
        let target = to.join(entry.file_name());
        if target.exists() {
            continue;
        }
        if std::fs::create_dir_all(to).is_err() {
            return;
        }
        if std::fs::rename(entry.path(), &target).is_ok() {
            moved += 1;
        }
    }
    if moved > 0 {
        // Only prunes when it empties out; anything left is someone else's.
        let _ = std::fs::remove_dir(from);
        tracing::info!(
            from = %from.display(),
            to = %to.display(),
            moved,
            "{}",
            crate::i18n::text(
                "moved files from a stale managed output directory",
                "已把过时输出目录里的文件搬到新位置",
            )
        );
    }
}

pub(crate) fn default_image_generation_timeout() -> u64 {
    180
}

pub(crate) fn default_kb_max_search_results() -> usize {
    5
}

pub(crate) fn default_kb_snippet_context_chars() -> usize {
    240
}

pub(crate) fn default_kb_proximity_window_chars() -> usize {
    512
}

pub(crate) fn default_kb_max_read_lines() -> usize {
    200
}

pub(crate) fn default_kb_max_file_size_kb() -> usize {
    1024
}

pub(crate) fn default_kb_allowed_extensions() -> String {
    ".txt,.md,.json,.jsonc,.json5,.yaml,.yml,.csv,.log,.py,.js,.ts,.jsx,.tsx,.mjs,.cjs,.html,.css,.scss,.sass,.less,.cfg,.ini,.conf,.toml,.kdl,.desktop,.service,.timer,.socket,.target,.mount,.rules,.network,.netdev,.properties,.hjson,.ron,.rst,.xml,.sh,.bash,.zsh,.fish,.nu,.ps1,.lua,.nix,.rasi,.yuck,.sql,.rs,.go,.c,.h,.cpp,.hpp,.java,.kt,.php,.rb,.pl,.org,.adoc,.tex".to_string()
}

pub(crate) fn default_kb_allowed_filenames() -> String {
    ".env,.env.local,.env.example,.env.sample,.envrc,.editorconfig,.gitignore,.gitattributes,.npmrc,.vimrc,.bashrc,.zshrc,.profile,.xinitrc,.xresources,config,dockerfile,containerfile,makefile,justfile,procfile,pkgbuild".to_string()
}

pub(crate) fn default_kb_semantic_chunk_chars() -> usize {
    512
}

pub(crate) fn default_kb_semantic_chunk_overlap() -> usize {
    80
}

pub(crate) fn default_kb_semantic_top_k() -> usize {
    5
}

pub(crate) fn default_kb_semantic_min_score() -> f32 {
    0.25
}

pub(crate) fn default_kb_keyword_strong_score_threshold() -> f32 {
    180.0
}

pub(crate) fn default_kb_embedding_timeout_seconds() -> u64 {
    60
}

pub(crate) fn default_diagnostics_timeout() -> u64 {
    5
}

pub(crate) fn default_diagnostics_max_stdout_chars() -> usize {
    8_000
}

pub(crate) fn default_diagnostics_max_stderr_chars() -> usize {
    4_000
}

pub(crate) fn default_calculator_backend() -> String {
    "rust-simple".to_string()
}

/// Compact trigger watermark. 0.8 (was 0.9) leaves room between the trigger
/// and the force watermark for the cheap mechanical layer to act first.
pub(crate) fn default_tool_output_spill_bytes() -> usize {
    50_000
}

pub(crate) fn default_trim_at_ratio() -> f32 {
    0.8
}

pub(crate) fn default_compact_force_ratio() -> f32 {
    0.9
}

pub(crate) fn default_compact_soft_ratio() -> f32 {
    0.5
}

pub(crate) fn default_compact_snip_ratio() -> f32 {
    0.6
}

pub(crate) fn default_cold_prune_after_minutes() -> u64 {
    1440
}

pub(crate) fn default_trim_batch_ratio() -> f32 {
    0.15
}

pub(crate) fn default_on_overflow() -> String {
    "compact".to_string()
}
