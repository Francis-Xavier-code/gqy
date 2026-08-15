use anyhow::{bail, Result};
use serde::de::DeserializeOwned;

pub(super) const MAX_HTML_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

pub(super) async fn read_text(response: reqwest::Response, max_bytes: usize) -> Result<String> {
    let bytes = read_bytes(response, max_bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) async fn read_json<T>(response: reqwest::Response, max_bytes: usize) -> Result<T>
where
    T: DeserializeOwned,
{
    let bytes = read_bytes(response, max_bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) async fn read_bytes(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        bail!("response too large (exceeds configured byte limit)")
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            bail!("response too large (exceeds configured byte limit)")
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
