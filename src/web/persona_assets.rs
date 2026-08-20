//! persona_assets — 个人头像与用户资产（自 src/web/sessions.rs 拆分）。

pub(crate) use super::*;

pub(crate) async fn persona_avatar(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let (config, prompts) = {
        let manager = state.manager.lock().unwrap();
        let prompts =
            read_prompt_documents(&manager.config, &state.paths).map_err(ApiError::internal)?;
        (manager.config.clone(), prompts)
    };
    let path = if let Some(path) = query.get("path").filter(|p| !p.is_empty()) {
        managed_persona_asset_path(&state.paths, path).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid managed persona asset path",
            )
        })?
    } else if query.contains_key("board") {
        active_persona_board_path(&config, &prompts, &state.paths)
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "persona board image not found"))?
    } else if let Some(path) = active_persona_avatar_path(&config, &prompts, &state.paths) {
        path
    } else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "persona avatar not found",
        ));
    };
    if path.starts_with(state.paths.persona_avatars_dir()) {
        validate_managed_persona_asset_file(&state.paths, &path)
            .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "persona avatar not found"))?;
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "persona avatar not found"))?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "persona avatar is too large",
        ));
    }
    let format = image::guess_format(&bytes)
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "persona avatar is not an image"))?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Bmp => "image/bmp",
        _ => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "persona avatar format is unsupported",
            ))
        }
    };
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub(crate) async fn upload_persona_asset(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Json<Value>, ApiError> {
    require_mutation(&headers, &state)?;
    if body.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "image is empty"));
    }
    if body.len() > PERSONA_ASSET_LIMIT {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "persona image is too large",
        ));
    }
    let format = image::guess_format(&body)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "unsupported image format"))?;
    let extension = match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::Gif => "gif",
        image::ImageFormat::WebP => "webp",
        image::ImageFormat::Bmp => "bmp",
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "unsupported image format",
            ))
        }
    };
    let hash = format!("{:x}", Sha256::digest(&body));
    let relative = format!("persona-avatars/{hash}.{extension}");
    let directory = state.paths.persona_avatars_dir();
    let destination = directory.join(format!("{hash}.{extension}"));
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(ApiError::internal)?;
    let directory_metadata = tokio::fs::symlink_metadata(&directory)
        .await
        .map_err(ApiError::internal)?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "persona asset directory is unsafe",
        ));
    }
    store_persona_asset(&directory, &destination, &hash, &body).await?;
    let config = state.manager.lock().unwrap().config.clone();
    if let Ok(prompts) = read_prompt_documents(&config, &state.paths) {
        cleanup_persona_assets(&state.paths, &prompts, &prompts);
    }
    Ok(Json(json!({
        "path": relative,
        "preview_url": format!("/api/persona/avatar?path={relative}"),
    })))
}

pub(crate) async fn store_persona_asset(
    directory: &FilePath,
    destination: &FilePath,
    expected_hash: &str,
    body: &[u8],
) -> std::result::Result<(), ApiError> {
    let replace_corrupt = match tokio::fs::symlink_metadata(destination).await {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            match verify_persona_asset_hash(destination, expected_hash).await {
                Ok(()) => return Ok(()),
                Err(error) if error.status == StatusCode::CONFLICT => true,
                Err(error) => return Err(error),
            }
        }
        Ok(_) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "persona asset destination is unsafe",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(ApiError::internal(error)),
    };

    let temporary = directory.join(format!(
        ".upload-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await
        .map_err(ApiError::internal)?;
    let write_result = async {
        file.write_all(body).await?;
        file.sync_all().await?;
        if replace_corrupt {
            tokio::fs::rename(&temporary, destination).await
        } else {
            tokio::fs::hard_link(&temporary, destination).await
        }
    }
    .await;
    match write_result {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            let directory = tokio::fs::File::open(directory)
                .await
                .map_err(ApiError::internal)?;
            directory.sync_all().await.map_err(ApiError::internal)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = tokio::fs::remove_file(&temporary).await;
            verify_persona_asset_hash(destination, expected_hash).await
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            Err(ApiError::internal(error))
        }
    }
}

pub(crate) async fn verify_persona_asset_hash(
    path: &FilePath,
    expected_hash: &str,
) -> std::result::Result<(), ApiError> {
    let bytes = tokio::fs::read(path).await.map_err(ApiError::internal)?;
    if bytes.len() > PERSONA_ASSET_LIMIT || format!("{:x}", Sha256::digest(&bytes)) != expected_hash
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "persona asset cache entry is corrupted",
        ));
    }
    Ok(())
}

pub(crate) fn text_asset(content: &'static str, content_type: &'static str) -> Response {
    asset_response(content.as_bytes(), content_type)
}

pub(crate) fn binary_asset(content: &'static [u8], content_type: &'static str) -> Response {
    asset_response(content, content_type)
}

pub(crate) fn asset_response(content: &'static [u8], content_type: &'static str) -> Response {
    finish_asset_response(content.into_response(), content_type)
}

pub(crate) fn finish_asset_response(
    mut response: Response,
    content_type: &'static str,
) -> Response {
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response.headers_mut().insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self'; media-src 'self' https: http:; style-src 'self'; script-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}
