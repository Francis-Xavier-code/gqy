//! admin — 自 src/web.rs 拆分。

use super::*;

pub(crate) async fn reset_platform_persona_state(
    state: &DaemonState,
    config: &AppConfig,
) -> std::result::Result<usize, PlatformPersonaResetError> {
    let persona = config.active_persona_scope();
    let session_ids = state
        .state_store
        .persona_reset_session_ids(&persona, "onebot")
        .map_err(|error| PlatformPersonaResetError::Internal(safe_error_message(error)))?;
    let bindings = state
        .state_store
        .platform_session_bindings(&persona, "onebot")
        .map_err(|error| PlatformPersonaResetError::Internal(safe_error_message(error)))?;
    let targets = session_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();

    {
        let mut manager = state.manager.lock().unwrap();
        if manager.admin_busy {
            return Err(PlatformPersonaResetError::Busy);
        }
        manager.admin_busy = true;
        for run in manager
            .active_runs
            .values()
            .filter(|run| targets.contains(&*run.session_id))
        {
            run.request_cancel();
        }
    }

    let tickets = session_ids
        .iter()
        .map(|session_id| state.platforms.preempt_session_turns(session_id))
        .collect::<Vec<_>>();
    let leases = match tokio::time::timeout(Duration::from_secs(10), async {
        let mut leases = Vec::with_capacity(tickets.len());
        for ticket in tickets {
            leases.push(ticket.acquire().await.expect("exclusive platform ticket"));
        }
        leases
    })
    .await
    {
        Ok(leases) => leases,
        Err(_) => {
            release_admin(&state.manager);
            return Err(PlatformPersonaResetError::Busy);
        }
    };

    for _ in 0..200 {
        let running = state
            .manager
            .lock()
            .unwrap()
            .active_runs
            .values()
            .any(|run| targets.contains(&*run.session_id));
        if !running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    if state
        .manager
        .lock()
        .unwrap()
        .active_runs
        .values()
        .any(|run| targets.contains(&*run.session_id))
    {
        drop(leases);
        release_admin(&state.manager);
        return Err(PlatformPersonaResetError::Busy);
    }

    let plugins = match state.platforms.plugins() {
        Ok(plugins) => plugins,
        Err(error) => {
            drop(leases);
            release_admin(&state.manager);
            return Err(PlatformPersonaResetError::Internal(safe_error_message(
                error,
            )));
        }
    };
    let reset_context = crate::platforms::plugins::PlatformPersonaResetContext {
        config,
        paths: &state.paths,
        bindings: &bindings,
    };
    if let Err(error) = plugins.after_persona_reset(&reset_context).await {
        drop(leases);
        release_admin(&state.manager);
        return Err(PlatformPersonaResetError::Internal(safe_error_message(
            error,
        )));
    }

    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ResetPersonaState {
            config: Box::new(config.clone()),
            reply,
        })
        .is_err()
    {
        drop(leases);
        release_admin(&state.manager);
        return Err(PlatformPersonaResetError::Unavailable);
    }
    let result = match receiver.await {
        Ok(Ok(())) => Ok(session_ids.len()),
        Ok(Err(AdminFailure::Invalid(message) | AdminFailure::Internal(message))) => {
            Err(PlatformPersonaResetError::Internal(message))
        }
        Err(_) => {
            release_admin(&state.manager);
            Err(PlatformPersonaResetError::Unavailable)
        }
    };
    drop(leases);
    result
}

/// Light admin reservation (session/model updates): serializes against other
/// admin operations but is allowed while turns are running.
pub(crate) fn reserve_admin_light(manager: &Arc<Mutex<ManagerState>>) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if manager.admin_busy {
        return Err(ApiError::new(StatusCode::CONFLICT, ipc::ADMIN_BUSY_MESSAGE));
    }
    manager.admin_busy = true;
    Ok(())
}

pub(crate) fn require_no_running_turn(state_store: &StateStore) -> std::result::Result<(), ApiError> {
    if state_store
        .has_any_running_turns()
        .map_err(ApiError::internal)?
    {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "a conversation turn is already running",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn release_admin(manager: &Arc<Mutex<ManagerState>>) {
    manager.lock().unwrap().admin_busy = false;
}

pub(crate) fn config_response(
    config: &AppConfig,
    context: ContextSnapshot,
    paths: &GQYPaths,
) -> std::result::Result<ConfigResponse, ApiError> {
    let mut redacted = config.clone();
    let mut secret_states = HashMap::new();
    for (index, provider) in redacted.providers.iter_mut().enumerate() {
        secret_states.insert(
            format!("providers.{index}.api_key"),
            provider
                .api_key
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        );
        provider.api_key = None;
    }
    redact_secret_list(
        &mut secret_states,
        "plugins.web.tavily_api_keys",
        &mut redacted.plugins.web.tavily_api_keys,
    );
    redact_secret_list(
        &mut secret_states,
        "plugins.web.firecrawl_api_keys",
        &mut redacted.plugins.web.firecrawl_api_keys,
    );
    redact_secret_list(
        &mut secret_states,
        "plugins.web.anysearch_api_keys",
        &mut redacted.plugins.web.anysearch_api_keys,
    );
    redact_secret_list(
        &mut secret_states,
        "plugins.web.exa_api_keys",
        &mut redacted.plugins.web.exa_api_keys,
    );
    secret_states.insert(
        "plugins.exchange_rate.api_key".to_string(),
        !redacted.plugins.exchange_rate.api_key.trim().is_empty(),
    );
    redacted.plugins.exchange_rate.api_key.clear();
    secret_states.insert(
        "platforms.qq.access_token".to_string(),
        !redacted.platforms.qq.access_token.trim().is_empty(),
    );
    redacted.platforms.qq.access_token.clear();
    redact_secret_list(
        &mut secret_states,
        "plugins.image_generation.api_keys",
        &mut redacted.plugins.image_generation.api_keys,
    );
    redact_api_quota_provider(
        &mut secret_states,
        "plugins.api_quota.deepseek",
        &mut redacted.plugins.api_quota.deepseek,
    );
    redact_api_quota_provider(
        &mut secret_states,
        "plugins.api_quota.openrouter",
        &mut redacted.plugins.api_quota.openrouter,
    );
    let mut config_value = serde_json::to_value(&redacted).map_err(ApiError::internal)?;
    if let Value::Object(config_object) = &mut config_value {
        config_object.insert(
            "memory".to_string(),
            serde_json::to_value(redacted.memory_config()).map_err(ApiError::internal)?,
        );
    }
    let prompts = read_prompt_documents(config, paths).map_err(ApiError::internal)?;
    let persona = persona_identity(config, &prompts);
    Ok(ConfigResponse {
        config: config_value,
        secret_states,
        prompts,
        models: safe_models(config),
        multimodal_models: safe_multimodal_models(config),
        display: web_display_config(config),
        context,
        persona,
    })
}

pub(crate) fn persona_identity(config: &AppConfig, prompts: &PromptDocuments) -> PersonaIdentity {
    let active = config.prompt.active_persona.trim();
    if active.is_empty() {
        return PersonaIdentity {
            name: "GQY".to_string(),
            avatar_url: Some("/assets/gqy-logo.png".to_string()),
            board_image_url: Some("/assets/gqywallpaper.png".to_string()),
            board_title: DEFAULT_BOARD_TITLE.to_string(),
            board_subtitle: DEFAULT_BOARD_SUBTITLE.to_string(),
            starter_prompts: DEFAULT_STARTER_PROMPTS.map(str::to_string).to_vec(),
        };
    }
    let document = prompts
        .personas
        .iter()
        .find(|document| document.name == active);
    let avatar_url = document
        .and_then(|document| document.avatar_path.as_deref())
        .filter(|path| !path.trim().is_empty())
        .and_then(|_| Some("/api/persona/avatar".to_string()));
    let board_image_url = if document
        .and_then(|document| document.board_image_path.as_deref())
        .is_some_and(|path| !path.trim().is_empty())
    {
        Some("/api/persona/avatar?board=1".to_string())
    } else {
        None
    };
    let board_title = document
        .and_then(|document| document.board_title.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_BOARD_TITLE)
        .to_string();
    let board_subtitle = document
        .and_then(|document| document.board_subtitle.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_BOARD_SUBTITLE)
        .to_string();
    let configured_prompts = document.and_then(|document| document.starter_prompts.as_deref());
    let starter_prompts = DEFAULT_STARTER_PROMPTS
        .iter()
        .enumerate()
        .map(|(index, fallback)| {
            configured_prompts
                .and_then(|values| values.get(index))
                .filter(|value| !value.trim().is_empty())
                .map_or_else(|| (*fallback).to_string(), Clone::clone)
        })
        .collect();
    PersonaIdentity {
        name: active.strip_suffix(".md").unwrap_or(active).to_string(),
        avatar_url,
        board_image_url,
        board_title,
        board_subtitle,
        starter_prompts,
    }
}

pub(crate) fn active_persona_avatar_path(
    config: &AppConfig,
    prompts: &PromptDocuments,
    paths: &GQYPaths,
) -> Option<PathBuf> {
    let active = config.prompt.active_persona.trim();
    if active.is_empty() {
        return None;
    }
    let value = prompts
        .personas
        .iter()
        .find(|document| document.name == active)
        .and_then(|document| document.avatar_path.as_deref())?;
    resolve_persona_asset_path(paths, value)
}

pub(crate) fn active_persona_board_path(
    config: &AppConfig,
    prompts: &PromptDocuments,
    paths: &GQYPaths,
) -> Option<PathBuf> {
    let active = config.prompt.active_persona.trim();
    let value = prompts
        .personas
        .iter()
        .find(|document| document.name == active)
        .and_then(|document| document.board_image_path.as_deref())?;
    resolve_persona_asset_path(paths, value)
}

pub(crate) fn resolve_persona_asset_path(paths: &GQYPaths, value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if persona_asset_uses_managed_namespace(value) {
        return managed_persona_asset_path(paths, value);
    }
    let path = PathBuf::from(value);
    if let Some(path) = paths.migrated_resource_path(&path) {
        return Some(path);
    }
    Some(if path.is_absolute() {
        path
    } else {
        paths.config_dir.join(path)
    })
}

pub(crate) fn managed_persona_asset_path(paths: &GQYPaths, value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.contains('\\') || value.chars().any(char::is_control) {
        return None;
    }
    let mut components = std::path::Path::new(value).components();
    while matches!(
        components.clone().next(),
        Some(std::path::Component::CurDir)
    ) {
        components.next();
    }
    if !matches!(components.next(), Some(std::path::Component::Normal(name)) if name == "persona-avatars")
    {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in components {
        match component {
            std::path::Component::Normal(component) => normalized.push(component),
            _ => return None,
        }
    }
    if normalized.as_os_str().is_empty() {
        return None;
    }
    Some(paths.persona_avatars_dir().join(normalized))
}

pub(crate) fn persona_asset_uses_managed_namespace(value: &str) -> bool {
    std::path::Path::new(value)
        .components()
        .find(|component| !matches!(component, std::path::Component::CurDir))
        .is_some_and(|component| {
            matches!(component, std::path::Component::Normal(name) if name == "persona-avatars")
        })
}

pub(crate) fn validate_managed_persona_asset_file(paths: &GQYPaths, path: &FilePath) -> Result<()> {
    let root_path = paths.persona_avatars_dir();
    let root_metadata = std::fs::symlink_metadata(&root_path)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("managed persona asset directory is unsafe");
    }
    let root = std::fs::canonicalize(root_path)?;
    let canonical = std::fs::canonicalize(path)?;
    if !canonical.starts_with(&root) || !std::fs::metadata(&canonical)?.is_file() {
        bail!("managed persona asset escapes its resource directory");
    }
    Ok(())
}

pub(crate) fn redact_secret_list(states: &mut HashMap<String, bool>, key: &str, values: &mut Vec<String>) {
    states.insert(
        key.to_string(),
        values.iter().any(|value| !value.trim().is_empty()),
    );
    values.clear();
}

pub(crate) fn redact_api_quota_provider(
    states: &mut HashMap<String, bool>,
    prefix: &str,
    provider: &mut crate::config::ApiQuotaProviderConfig,
) {
    if provider.accounts.is_empty() {
        provider
            .accounts
            .push(crate::config::ApiQuotaAccountConfig {
                id: "account-1".to_string(),
                name: "默认账号".to_string(),
                api_key: provider.api_key.clone(),
            });
    } else if !provider.api_key.trim().is_empty() && provider.accounts[0].api_key.trim().is_empty()
    {
        provider.accounts[0].api_key = provider.api_key.clone();
    }
    provider.api_key.clear();
    let mut used_ids = HashSet::with_capacity(provider.accounts.len());
    for (index, account) in provider.accounts.iter_mut().enumerate() {
        if account.id.trim().is_empty() || !used_ids.insert(account.id.clone()) {
            let mut number = index + 1;
            loop {
                let candidate = format!("account-{number}");
                if used_ids.insert(candidate.clone()) {
                    account.id = candidate;
                    break;
                }
                number += 1;
            }
        }
    }
    for (index, account) in provider.accounts.iter_mut().enumerate() {
        let key = format!("{prefix}.accounts.{index}.api_key");
        states.insert(key, !account.api_key.trim().is_empty());
        account.api_key.clear();
    }
}

pub(crate) fn restore_config_secrets(
    candidate: &mut AppConfig,
    current: &AppConfig,
    mutations: &HashMap<String, SecretMutation>,
) -> std::result::Result<(), ApiError> {
    let mut recognized = HashSet::new();
    for (index, provider) in candidate.providers.iter_mut().enumerate() {
        let key = format!("providers.{index}.api_key");
        recognized.insert(key.clone());
        let existing = current
            .providers
            .iter()
            .find(|item| item.id == provider.id)
            .and_then(|item| item.api_key.clone());
        provider.api_key = match mutations.get(&key) {
            Some(SecretMutation::Set(value)) => normalize_single_secret(value, &key)?,
            Some(SecretMutation::Clear) => None,
            None => existing,
        };
    }

    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.web.tavily_api_keys",
        |config| &mut config.plugins.web.tavily_api_keys,
        |config| &config.plugins.web.tavily_api_keys,
    )?;
    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.web.firecrawl_api_keys",
        |config| &mut config.plugins.web.firecrawl_api_keys,
        |config| &config.plugins.web.firecrawl_api_keys,
    )?;
    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.web.anysearch_api_keys",
        |config| &mut config.plugins.web.anysearch_api_keys,
        |config| &config.plugins.web.anysearch_api_keys,
    )?;
    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.web.exa_api_keys",
        |config| &mut config.plugins.web.exa_api_keys,
        |config| &config.plugins.web.exa_api_keys,
    )?;
    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.image_generation.api_keys",
        |config| &mut config.plugins.image_generation.api_keys,
        |config| &config.plugins.image_generation.api_keys,
    )?;

    let exchange_key = "plugins.exchange_rate.api_key";
    recognized.insert(exchange_key.to_string());
    candidate.plugins.exchange_rate.api_key = match mutations.get(exchange_key) {
        Some(SecretMutation::Set(value)) => {
            normalize_single_secret(value, exchange_key)?.unwrap_or_default()
        }
        Some(SecretMutation::Clear) => String::new(),
        None => current.plugins.exchange_rate.api_key.clone(),
    };

    restore_api_quota_provider(
        &mut candidate.plugins.api_quota.deepseek,
        &current.plugins.api_quota.deepseek,
        mutations,
        &mut recognized,
        "plugins.api_quota.deepseek",
    )?;
    restore_api_quota_provider(
        &mut candidate.plugins.api_quota.openrouter,
        &current.plugins.api_quota.openrouter,
        mutations,
        &mut recognized,
        "plugins.api_quota.openrouter",
    )?;

    let onebot_token_key = "platforms.qq.access_token";
    recognized.insert(onebot_token_key.to_string());
    candidate.platforms.qq.access_token = match mutations.get(onebot_token_key) {
        Some(SecretMutation::Set(value)) => {
            normalize_single_secret(value, onebot_token_key)?.unwrap_or_default()
        }
        Some(SecretMutation::Clear) => String::new(),
        None => current.platforms.qq.access_token.clone(),
    };

    if let Some(key) = mutations.keys().find(|key| !recognized.contains(*key)) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("unknown secret field: {key}"),
        ));
    }
    Ok(())
}

pub(crate) fn restore_api_quota_provider(
    candidate: &mut crate::config::ApiQuotaProviderConfig,
    current: &crate::config::ApiQuotaProviderConfig,
    mutations: &HashMap<String, SecretMutation>,
    recognized: &mut HashSet<String>,
    prefix: &str,
) -> std::result::Result<(), ApiError> {
    for (index, account) in candidate.accounts.iter_mut().enumerate() {
        let key = format!("{prefix}.accounts.{index}.api_key");
        recognized.insert(key.clone());
        let mut existing = current
            .accounts
            .iter()
            .find(|item| !account.id.is_empty() && item.id == account.id)
            .or_else(|| {
                current
                    .accounts
                    .iter()
                    .find(|item| item.id.is_empty() && item.name == account.name)
            })
            .map(|item| item.api_key.clone())
            .or_else(|| {
                (index == 0 && current.accounts.is_empty()).then(|| current.api_key.clone())
            })
            .unwrap_or_default();
        if existing.is_empty() && index == 0 && !current.api_key.trim().is_empty() {
            existing = current.api_key.clone();
        }
        account.api_key = match mutations.get(&key) {
            Some(SecretMutation::Set(value)) => {
                normalize_single_secret(value, &key)?.unwrap_or_default()
            }
            Some(SecretMutation::Clear) => String::new(),
            None => existing,
        };
    }
    candidate.api_key.clear();
    Ok(())
}

pub(crate) fn restore_secret_list<Mut, Ref>(
    candidate: &mut AppConfig,
    current: &AppConfig,
    mutations: &HashMap<String, SecretMutation>,
    recognized: &mut HashSet<String>,
    key: &str,
    candidate_values: Mut,
    current_values: Ref,
) -> std::result::Result<(), ApiError>
where
    Mut: FnOnce(&mut AppConfig) -> &mut Vec<String>,
    Ref: FnOnce(&AppConfig) -> &Vec<String>,
{
    recognized.insert(key.to_string());
    *candidate_values(candidate) = match mutations.get(key) {
        Some(SecretMutation::Set(value)) => parse_secret_list(value, key)?,
        Some(SecretMutation::Clear) => Vec::new(),
        None => current_values(current).clone(),
    };
    Ok(())
}

pub(crate) fn normalize_single_secret(
    value: &str,
    field: &str,
) -> std::result::Result<Option<String>, ApiError> {
    validate_secret_text(value, field)?;
    Ok(Some(value.trim().to_string()).filter(|value| !value.is_empty()))
}

pub(crate) fn parse_secret_list(value: &str, field: &str) -> std::result::Result<Vec<String>, ApiError> {
    validate_secret_text(value, field)?;
    Ok(value
        .split(|character| matches!(character, ',' | '\n' | '\r'))
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect())
}

pub(crate) fn validate_secret_text(value: &str, field: &str) -> std::result::Result<(), ApiError> {
    if value.chars().count() > MAX_SECRET_CHARS
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} is invalid"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_config_candidate(config: &AppConfig) -> std::result::Result<(), ApiError> {
    config.validate().map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid configuration: {}", safe_error_message(error)),
        )
    })?;
    let mut provider_ids = HashSet::with_capacity(config.providers.len());
    for provider in &config.providers {
        if !provider_ids.insert(provider.id.trim()) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("duplicate provider id: {}", provider.id),
            ));
        }
    }
    if let Some(active) = &config.active_provider_models {
        let mut checked = config.clone();
        checked
            .set_active_provider_models(active)
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, safe_error_message(error)))?;
    }
    if let Some(active) = &config.active_multimodal_provider_models {
        let choices = config.multimodal_provider_model_choices();
        let mut seen = HashSet::with_capacity(active.len());
        for model in active {
            if !seen.insert((&model.provider_id, &model.model))
                || !choices.iter().any(|choice| {
                    choice.provider_id == model.provider_id && choice.model == model.model
                })
            {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "invalid multimodal provider/model: {} / {}",
                        model.provider_id, model.model
                    ),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_prompt_documents(
    config: &AppConfig,
    prompts: &PromptDocuments,
) -> std::result::Result<(), ApiError> {
    validate_prompt_document_list("persona", &prompts.personas)?;
    validate_prompt_document_list("identity", &prompts.identities)?;
    let mut persona_scopes = HashMap::<String, &str>::new();
    for document in &prompts.personas {
        if document.name.eq_ignore_ascii_case("system-prompt.md") {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "system-prompt.md is reserved and cannot be used as a persona",
            ));
        }
        let scope = crate::config::persona_scope_name(&document.name);
        if let Some(existing) = persona_scopes.insert(scope.clone(), &document.name) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "persona names map to the same persistent scope: {existing} and {} ({scope})",
                    document.name
                ),
            ));
        }
    }
    if !config.prompt.active_persona.trim().is_empty()
        && !prompts
            .personas
            .iter()
            .any(|document| document.name == config.prompt.active_persona)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "the active persona does not exist",
        ));
    }
    for route in &config.platforms.qq.conversations {
        let Some(name) = route.persona.custom_name() else {
            continue;
        };
        if !prompts
            .personas
            .iter()
            .any(|document| document.name == name)
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("QQ conversation persona does not exist: {name}"),
            ));
        }
    }
    if !config.prompt.active_identity.trim().is_empty()
        && !prompts
            .identities
            .iter()
            .any(|document| document.name == config.prompt.active_identity)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "the active identity does not exist",
        ));
    }
    Ok(())
}

pub(crate) fn reconcile_qq_persona_references(config: &mut AppConfig, prompts: &PromptDocuments) {
    let renames = prompts
        .personas
        .iter()
        .filter_map(|document| {
            document
                .original_name
                .as_deref()
                .filter(|original| *original != document.name)
                .map(|original| (original.to_string(), document.name.clone()))
        })
        .collect::<HashMap<_, _>>();
    for route in &mut config.platforms.qq.conversations {
        let Some(current) = route.persona.custom_name() else {
            continue;
        };
        if let Some(next) = renames.get(current) {
            route.persona = crate::config::PlatformPersonaOverride::Custom { name: next.clone() };
        }
    }
}

pub(crate) fn validate_prompt_document_list(
    kind: &str,
    documents: &[PromptDocument],
) -> std::result::Result<(), ApiError> {
    if documents.len() > MAX_PROMPT_DOCUMENTS {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("at most {MAX_PROMPT_DOCUMENTS} {kind} documents are allowed"),
        ));
    }
    let mut names = HashSet::with_capacity(documents.len());
    let mut original_names = HashSet::with_capacity(documents.len());
    for document in documents {
        validate_prompt_document_name(&document.name, kind)?;
        if !names.insert(document.name.as_str()) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("duplicate {kind} document: {}", document.name),
            ));
        }
        if document.content.chars().count() > MAX_PROMPT_DOCUMENT_CHARS
            || document.content.contains('\0')
        {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("{kind} document is too large: {}", document.name),
            ));
        }
        for (field, value) in [
            ("avatar", document.avatar_path.as_deref()),
            ("board image", document.board_image_path.as_deref()),
        ] {
            if value.is_some_and(|path| {
                path.len() > 4_096 || path.contains('\0') || path.trim() != path
            }) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid {kind} {field} path: {}", document.name),
                ));
            }
        }
        for (field, value) in [
            ("board title", document.board_title.as_deref()),
            ("board subtitle", document.board_subtitle.as_deref()),
        ] {
            if value.is_some_and(|text| {
                text.chars().count() > 200 || text.chars().any(char::is_control)
            }) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid {kind} {field}: {}", document.name),
                ));
            }
        }
        if let Some(prompts) = document.starter_prompts.as_deref() {
            if prompts.len() > 4
                || prompts
                    .iter()
                    .any(|text| text.chars().count() > 200 || text.chars().any(char::is_control))
            {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid {kind} starter prompts: {}", document.name),
                ));
            }
        }
        if let Some(original) = document.original_name.as_deref() {
            validate_prompt_document_name(original, kind)?;
            if !original_names.insert(original) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("duplicate original {kind} document: {original}"),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_prompt_document_name(name: &str, kind: &str) -> std::result::Result<(), ApiError> {
    let valid = name == name.trim()
        && name.ends_with(".md")
        && name.len() <= 240
        && name.len() > 3
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
        && FilePath::new(name)
            .file_name()
            .and_then(|value| value.to_str())
            == Some(name);
    if !valid {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid {kind} document name: {name}"),
        ));
    }
    Ok(())
}

pub(crate) fn read_prompt_documents(config: &AppConfig, paths: &GQYPaths) -> Result<PromptDocuments> {
    Ok(PromptDocuments {
        personas: read_prompt_document_dir(&config.prompts_dir_path(paths), true)?,
        identities: read_prompt_document_dir(&config.identities_dir_path(paths), false)?,
    })
}

pub(crate) fn read_prompt_document_dir(
    dir: &FilePath,
    with_avatar_metadata: bool,
) -> Result<Vec<PromptDocument>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut documents = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        if with_avatar_metadata && name.eq_ignore_ascii_case("system-prompt.md") {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())?;
        let metadata = with_avatar_metadata
            .then(|| read_prompt_metadata(&entry.path()))
            .flatten()
            .unwrap_or_default();
        documents.push(PromptDocument {
            original_name: Some(name.clone()),
            name,
            content,
            avatar_path: metadata.avatar_path,
            board_image_path: metadata.board_image_path,
            board_title: metadata.board_title,
            board_subtitle: metadata.board_subtitle,
            starter_prompts: metadata.starter_prompts,
        });
    }
    documents.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(documents)
}

pub(crate) fn read_prompt_metadata(path: &FilePath) -> Option<PersonaMetadata> {
    let sidecar = path.with_extension("json");
    let raw = std::fs::read_to_string(sidecar).ok()?;
    serde_json::from_str(&raw).ok()
}

pub(crate) fn prompt_configuration_changed(current: &AppConfig, candidate: &AppConfig) -> bool {
    serde_json::to_value(&current.prompt).ok() != serde_json::to_value(&candidate.prompt).ok()
        || current.system_prompt_file != candidate.system_prompt_file
        || current.system_prompt != candidate.system_prompt
}

pub(crate) fn prompt_documents_changed(current: &PromptDocuments, candidate: &PromptDocuments) -> bool {
    canonical_prompt_documents(&current.personas) != canonical_prompt_documents(&candidate.personas)
        || canonical_prompt_documents(&current.identities)
            != canonical_prompt_documents(&candidate.identities)
}

pub(crate) fn canonical_prompt_documents(documents: &[PromptDocument]) -> Vec<(String, String)> {
    let mut values = documents
        .iter()
        .map(|document| (document.name.clone(), document.content.clone()))
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(&right.0));
    values
}

pub(crate) struct FileBackup {
    path: PathBuf,
    content: Option<Vec<u8>>,
}

pub(crate) struct PersonaScopeBackup {
    original: PathBuf,
    staged: PathBuf,
    destination: Option<PathBuf>,
}

pub(crate) struct PersonaDbRenameGuard {
    state: StateStore,
    renames: Vec<(String, String)>,
    committed: bool,
}

impl PersonaDbRenameGuard {
    pub(crate) fn new(state: StateStore, changes: &[(String, Option<String>)]) -> Result<Self> {
        let renames = changes
            .iter()
            .filter_map(|(old_name, new_name)| {
                let new_name = new_name.as_deref()?;
                let old_scope = crate::config::persona_scope_name(old_name);
                let new_scope = crate::config::persona_scope_name(new_name);
                (old_scope != new_scope).then_some((old_scope, new_scope))
            })
            .collect::<Vec<_>>();
        migrate_persona_db_scopes(&state, &renames)?;
        Ok(Self {
            state,
            renames,
            committed: false,
        })
    }

    pub(crate) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for PersonaDbRenameGuard {
    pub(crate) fn drop(&mut self) {
        if self.committed || self.renames.is_empty() {
            return;
        }
        let reverse = self
            .renames
            .iter()
            .map(|(old, new)| (new.clone(), old.clone()))
            .collect::<Vec<_>>();
        let _ = migrate_persona_db_scopes(&self.state, &reverse);
    }
}

pub(crate) fn migrate_persona_db_scopes(state: &StateStore, renames: &[(String, String)]) -> Result<()> {
    let staged = renames
        .iter()
        .map(|(old, new)| {
            (
                old.clone(),
                format!("persona-migration-{}", random_token(18)),
                new.clone(),
            )
        })
        .collect::<Vec<_>>();
    for (staged_count, (old, temporary, _)) in staged.iter().enumerate() {
        if let Err(error) = state.rename_persona_scope(old, temporary) {
            for (old, temporary, _) in staged[..staged_count].iter().rev() {
                let _ = state.rename_persona_scope(temporary, old);
            }
            return Err(error);
        }
    }
    for (finalized, (_, temporary, new)) in staged.iter().enumerate() {
        if let Err(error) = state.rename_persona_scope(temporary, new) {
            for (_, temporary, new) in staged[..finalized].iter().rev() {
                let _ = state.rename_persona_scope(new, temporary);
            }
            for (old, temporary, _) in staged.iter().rev() {
                let _ = state.rename_persona_scope(temporary, old);
            }
            return Err(error);
        }
    }
    Ok(())
}

pub(crate) fn apply_prompt_documents(
    current_config: &AppConfig,
    next_config: &AppConfig,
    current: &PromptDocuments,
    next: &PromptDocuments,
    paths: &GQYPaths,
) -> Result<Vec<FileBackup>> {
    let mut mutations = HashMap::<PathBuf, Option<Vec<u8>>>::new();
    collect_prompt_file_mutations(
        &current.personas,
        &next.personas,
        &current_config.prompts_dir_path(paths),
        &next_config.prompts_dir_path(paths),
        &mut mutations,
        true,
    );
    collect_prompt_file_mutations(
        &current.identities,
        &next.identities,
        &current_config.identities_dir_path(paths),
        &next_config.identities_dir_path(paths),
        &mut mutations,
        false,
    );
    let backups = mutations
        .keys()
        .map(|path| FileBackup {
            path: path.clone(),
            content: std::fs::read(path).ok(),
        })
        .collect::<Vec<_>>();
    for (path, content) in mutations {
        let result = if let Some(content) = content {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content)
        } else if path.exists() {
            std::fs::remove_file(&path)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            restore_file_backups(&backups);
            return Err(error.into());
        }
    }
    Ok(backups)
}

pub(crate) fn apply_persona_scope_changes(
    current_config: &AppConfig,
    next_config: &AppConfig,
    current: &PromptDocuments,
    next: &PromptDocuments,
    paths: &GQYPaths,
) -> Result<Vec<PersonaScopeBackup>> {
    let changes = persona_document_changes(current, next);
    let mut backups = Vec::new();
    let stage_result = (|| -> Result<()> {
        for (change_index, (old_name, new_name)) in changes.iter().enumerate() {
            let old_paths = [
                current_config.persona_memory_data_dir(paths, old_name),
                current_config.persona_memory_state_dir(paths, old_name),
                current_config.persona_skills_dir(paths, old_name),
            ];
            let new_paths = new_name.as_ref().map(|name| {
                [
                    next_config.persona_memory_data_dir(paths, name),
                    next_config.persona_memory_state_dir(paths, name),
                    next_config.persona_skills_dir(paths, name),
                ]
            });
            for (scope_index, original) in old_paths.into_iter().enumerate() {
                if !original.exists() {
                    continue;
                }
                let parent = original
                    .parent()
                    .context("persona scope path has no parent")?;
                let staged = parent.join(format!(
                    ".gqy-web-scope-{}-{change_index}-{scope_index}",
                    random_token(10)
                ));
                std::fs::rename(&original, &staged)?;
                backups.push(PersonaScopeBackup {
                    original,
                    staged,
                    destination: new_paths.as_ref().map(|paths| paths[scope_index].clone()),
                });
            }
        }
        Ok(())
    })();
    if let Err(error) = stage_result {
        restore_persona_scope_backups(&backups);
        return Err(error);
    }

    let result = (|| -> Result<()> {
        for backup in &backups {
            let Some(destination) = &backup.destination else {
                continue;
            };
            if destination.exists() {
                anyhow::bail!(
                    "persona scope destination already exists: {}",
                    destination.display()
                );
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&backup.staged, destination)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        restore_persona_scope_backups(&backups);
        return Err(error);
    }
    Ok(backups)
}

/// True when applying `next` cannot safely coexist with running turns:
/// persona renames/deletions and active-persona switches migrate or delete
/// the session state those turns are using. Everything else hot-applies.
pub(crate) fn config_change_requires_interrupt(
    current: &AppConfig,
    next: &AppConfig,
    paths: &GQYPaths,
    next_prompts: &PromptDocuments,
) -> bool {
    let Ok(previous_prompts) = read_prompt_documents(current, paths) else {
        // The safe direction: interrupt when the current prompt layout cannot
        // be read to prove the change is turn-safe.
        return true;
    };
    if !persona_document_changes(&previous_prompts, next_prompts).is_empty() {
        return true;
    }
    current.active_persona_scope() != next.active_persona_scope()
}

pub(crate) fn persona_document_changes(
    current: &PromptDocuments,
    next: &PromptDocuments,
) -> Vec<(String, Option<String>)> {
    let mut changes = Vec::new();
    for document in &current.personas {
        let represented = next.personas.iter().find(|next_document| {
            next_document.original_name.as_deref() == Some(document.name.as_str())
                || next_document.original_name.is_none() && next_document.name == document.name
        });
        match represented {
            Some(next_document) if next_document.name != document.name => {
                changes.push((document.name.clone(), Some(next_document.name.clone())));
            }
            None => changes.push((document.name.clone(), None)),
            _ => {}
        }
    }
    changes
}

pub(crate) fn restore_persona_scope_backups(backups: &[PersonaScopeBackup]) {
    for backup in backups.iter().rev() {
        if let Some(destination) = &backup.destination {
            if destination.exists() && !backup.staged.exists() {
                let _ = std::fs::rename(destination, &backup.staged);
            }
        }
        if backup.staged.exists() {
            if let Some(parent) = backup.original.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::rename(&backup.staged, &backup.original);
        }
    }
}

pub(crate) fn finalize_persona_scope_backups(backups: &[PersonaScopeBackup]) {
    for backup in backups {
        if backup.destination.is_none() && backup.staged.exists() {
            let _ = std::fs::remove_dir_all(&backup.staged);
        }
    }
}

pub(crate) fn collect_prompt_file_mutations(
    current: &[PromptDocument],
    next: &[PromptDocument],
    current_dir: &FilePath,
    next_dir: &FilePath,
    mutations: &mut HashMap<PathBuf, Option<Vec<u8>>>,
    with_avatar_metadata: bool,
) {
    for document in next {
        let content = document.content.trim_end();
        let content = if content.is_empty() {
            Vec::new()
        } else {
            format!("{content}\n").into_bytes()
        };
        mutations.insert(next_dir.join(&document.name), Some(content));
        if with_avatar_metadata {
            let metadata_path = next_dir.join(&document.name).with_extension("json");
            let metadata = PersonaMetadata {
                avatar_path: document
                    .avatar_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_string),
                board_image_path: document
                    .board_image_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_string),
                board_title: document
                    .board_title
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                board_subtitle: document
                    .board_subtitle
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                starter_prompts: document.starter_prompts.clone(),
            };
            let metadata = if metadata.avatar_path.is_none()
                && metadata.board_image_path.is_none()
                && metadata.board_title.is_none()
                && metadata.board_subtitle.is_none()
                && metadata.starter_prompts.is_none()
            {
                None
            } else {
                Some(
                    serde_json::to_vec_pretty(&metadata)
                        .expect("serializing persona metadata cannot fail"),
                )
            };
            mutations.insert(
                metadata_path,
                metadata.map(|mut bytes| {
                    bytes.push(b'\n');
                    bytes
                }),
            );
        }
    }
    for document in current {
        let represented = next.iter().find(|next_document| {
            next_document.original_name.as_deref() == Some(document.name.as_str())
                || next_document.original_name.is_none() && next_document.name == document.name
        });
        let old_path = current_dir.join(&document.name);
        let retained_at_same_path = represented
            .map(|next_document| next_dir.join(&next_document.name) == old_path)
            .unwrap_or(false);
        if !retained_at_same_path {
            mutations.entry(old_path).or_insert(None);
            if with_avatar_metadata {
                mutations
                    .entry(current_dir.join(&document.name).with_extension("json"))
                    .or_insert(None);
            }
        }
    }
}

pub(crate) fn restore_file_backups(backups: &[FileBackup]) {
    for backup in backups {
        restore_optional_file(&backup.path, backup.content.as_deref());
    }
}

pub(crate) fn restore_optional_file(path: &FilePath, content: Option<&[u8]>) {
    if let Some(content) = content {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, content);
    } else if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

pub(crate) fn safe_models(config: &AppConfig) -> Vec<SafeModel> {
    config
        .provider_model_choices()
        .into_iter()
        .map(|choice| SafeModel {
            active: config.is_active_provider_model(&choice.provider_id, &choice.model),
            provider_id: choice.provider_id,
            provider_name: choice.provider_name,
            model: choice.model,
        })
        .collect()
}

pub(crate) fn web_display_config(config: &AppConfig) -> WebDisplayConfig {
    let mixed_model_endpoint_display = config.display.mixed_model_endpoint_display.clone();
    WebDisplayConfig {
        reasoning: config.display.reasoning.clone(),
        tool_calls: config.display.tool_calls.clone(),
        readable_tool_names: config.display.readable_tool_names,
        command_output_lines: config.display.command_output_lines,
        show_mixed_model_endpoint: config.active_provider_model_choices().len() > 1
            && matches!(mixed_model_endpoint_display.as_str(), "interactive" | "all"),
        mixed_model_endpoint_display,
    }
}

pub(crate) fn safe_multimodal_models(config: &AppConfig) -> Vec<SafeModel> {
    config
        .multimodal_provider_model_choices()
        .into_iter()
        .map(|choice| SafeModel {
            active: config.is_active_multimodal_provider_model(&choice.provider_id, &choice.model),
            provider_id: choice.provider_id,
            provider_name: choice.provider_name,
            model: choice.model,
        })
        .collect()
}

impl SafeTurn {
    pub(crate) fn from_turn(turn: Turn, assets: Vec<ImageAsset>, artifacts: Vec<ArtifactAsset>) -> Self {
        let assets = assets
            .into_iter()
            .map(|asset| {
                let hide_caption = meme_asset_caption_hidden(&asset, &turn.tool_reports);
                SafeImageAsset::from_asset(asset, hide_caption)
            })
            .collect();
        Self {
            id: turn.turn_id,
            seq: turn.seq,
            status: match turn.status {
                TurnStatus::Running => "running",
                TurnStatus::Completed => "completed",
                TurnStatus::Interrupted => "interrupted",
            },
            active_context: !turn.hidden,
            user_content: turn.display_content,
            assistant_content: redact_internal_assistant_text(&turn.assistant_content),
            assistant_reasoning: turn
                .assistant_reasoning
                .map(|reasoning| redact_internal_assistant_text(&reasoning)),
            provider_id: turn.assistant_provider_id,
            model: turn.assistant_model,
            user_timestamp: turn.user_timestamp,
            assistant_timestamp: turn.assistant_timestamp,
            token_total: turn.token_total,
            token_prompt: turn.token_prompt,
            token_cache_read: turn.token_cache_read,
            token_usage_estimated: turn.token_usage_estimated,
            question_exchanges: turn.question_exchanges,
            followups: turn.followups.into_iter().map(SafeFollowup::from).collect(),
            assets,
            artifacts: artifacts.into_iter().map(SafeArtifactAsset::from).collect(),
            attachments: turn
                .attachments
                .into_iter()
                .map(SafeUserAttachment::from)
                .collect(),
            revision: turn.revision,
        }
    }
}

