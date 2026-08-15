//! tests — 自 src/tools/web_images.rs 外移。
#![cfg(test)]

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(title: &str, rank: usize, width: u32, height: u32) -> ImageCandidate {
        ImageCandidate {
            title: title.to_string(),
            page_url: "https://example.com/page".to_string(),
            image_url: format!("https://example.com/{rank}.jpg"),
            thumbnail_url: String::new(),
            source: "test".to_string(),
            width,
            height,
            search_description: String::new(),
            provider_rank: rank,
        }
    }

    fn provider() -> ProviderConfig {
        ProviderConfig {
            id: "vision".to_string(),
            display_name: "Vision".to_string(),
            base_url: "https://example.com/v1".to_string(),
            protocol: "openai-chat".to_string(),
            api_key: None,
            models: vec!["vision-model".to_string()],
            model_context_window: HashMap::new(),
            model_temperature: HashMap::new(),
            model_modalities: HashMap::new(),
            model_costs: HashMap::new(),
            default_model: "vision-model".to_string(),
            timeout_seconds: 60,
            temperature: 0.2,
            anthropic_max_tokens: 4096,
            extra_body: None,
        }
    }

    fn stored(path: PathBuf, rank: usize) -> StoredImage {
        StoredImage {
            candidate: candidate("test image", rank, 2, 2),
            local_path: path,
            mime_type: "image/png".to_string(),
            size_bytes: 16,
            sha256: format!("hash-{rank}"),
            used_thumbnail: false,
            vision: VisionScreening::not_requested(),
        }
    }

    #[test]
    fn extracts_ddg_vqd() {
        assert_eq!(
            extract_ddg_vqd("foo vqd=\"123-456\" bar"),
            Some("123-456".to_string())
        );
        assert_eq!(extract_ddg_vqd("foo"), None);
    }

    #[test]
    fn detects_png_dimensions() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&32u32.to_be_bytes());
        bytes.extend_from_slice(&16u32.to_be_bytes());
        assert_eq!(detect_image_dimensions(&bytes, "image/png"), (32, 16));
        assert_eq!(
            detect_image_mime(b"<html>not an image</html>", "image/png", "photo.png"),
            None
        );
    }

    #[test]
    fn exact_model_number_outranks_wrong_high_resolution_model() {
        let query = "华为 Mate 70 Pro 绿色 背面";
        let correct = candidate("华为 Mate 70 Pro 云杉绿 背面", 3, 1000, 800);
        let wrong = candidate("华为 Mate 30 Pro 5G 绿色背面", 1, 3000, 2000);
        assert!(score_candidate(query, &correct) > score_candidate(query, &wrong));
    }

    #[test]
    fn requested_product_outranks_accessory() {
        let query = "华为 Mate 70 Pro 绿色 背面";
        let product = candidate("华为 Mate 70 Pro 云杉绿手机背面", 3, 1000, 800);
        let case = candidate("华为 Mate 70 Pro 绿色手机壳保护套", 1, 3000, 3000);
        assert!(score_candidate(query, &product) > score_candidate(query, &case));
    }

    #[test]
    fn cjk_query_adds_subterms_without_spaces() {
        let terms = image_query_terms("杭州西湖断桥残雪实景");
        assert!(terms.contains(&"断桥".to_string()));
        assert!(terms.contains(&"残雪".to_string()));
    }

    #[test]
    fn blocks_local_and_private_image_urls() {
        for url in [
            "http://localhost/image.png",
            "http://127.0.0.1/image.png",
            "http://10.0.0.1/image.png",
            "http://[::1]/image.png",
            "http://[::ffff:127.0.0.1]/image.png",
        ] {
            assert!(!is_safe_remote_url(&Url::parse(url).unwrap()), "{url}");
        }
        assert!(is_safe_remote_url(
            &Url::parse("https://images.example.com/photo.jpg").unwrap()
        ));
    }

    #[test]
    fn incomplete_vision_batch_fails_closed() {
        let screenings = parse_vision_screenings(
            r#"{"items":[{"id":1,"relevance":90,"quality":80,"safe":true,"description":"匹配","reason":"主体正确"}]}"#,
            &provider(),
            2,
        );
        assert!(screenings[0].accepted);
        assert!(screenings[0].safe);
        assert!(!screenings[1].accepted);
        assert!(!screenings[1].safe);
        assert!(!parse_safe_bool(Some(&Value::String("unsafe".to_string()))));
        assert!(!parse_safe_bool(Some(&Value::String(
            "not safe".to_string()
        ))));
    }

    #[test]
    fn parses_provider_result_shapes() {
        let ddg = parse_ddg_results(
            r#"{"results":[{"title":"cat","url":"https://example.com/page","image":"https://example.com/cat.jpg","thumbnail":"https://example.com/cat-small.jpg","width":800,"height":600}]}"#,
            5,
        )
        .unwrap();
        assert_eq!(ddg.len(), 1);
        let bing = parse_bing_results(
            r#"<a class="iusc" m="{&quot;t&quot;:&quot;cat&quot;,&quot;purl&quot;:&quot;https://example.com/page&quot;,&quot;murl&quot;:&quot;https://example.com/cat.jpg&quot;,&quot;turl&quot;:&quot;https://example.com/cat-small.jpg&quot;}"></a>"#,
            5,
        );
        assert_eq!(bing.len(), 1);
    }

    #[test]
    fn provider_mode_selects_mainland_sources() {
        let mut config = AppConfig::default();
        config.plugins.web_images.source_mode = "mainland".to_string();
        config.plugins.web.searxng_base_url.clear();
        let ids = image_search_providers(&config, "猫", true, true)
            .into_iter()
            .map(ImageSearchProvider::id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["bing_cn", "baidu", "so360"]);

        let safe_without_vision = image_search_providers(&config, "猫", true, false)
            .into_iter()
            .map(ImageSearchProvider::id)
            .collect::<Vec<_>>();
        assert_eq!(safe_without_vision, vec!["bing_cn"]);
    }

    #[test]
    fn legacy_web_images_config_defaults_source_mode() {
        let config: crate::config::WebImagesPluginConfig =
            serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(config.source_mode, "auto");
    }

    #[tokio::test]
    async fn contact_sheet_skips_corrupt_images() {
        let dir = tempfile::tempdir().unwrap();
        let corrupt_path = dir.path().join("corrupt.png");
        tokio::fs::write(&corrupt_path, b"not an image")
            .await
            .unwrap();
        let valid_path = dir.path().join("valid.png");
        RgbImage::from_pixel(2, 2, Rgb([255, 0, 0]))
            .save(&valid_path)
            .unwrap();

        let (_, included) =
            contact_sheet_data_url(&[stored(corrupt_path, 1), stored(valid_path, 2)])
                .await
                .unwrap();
        assert_eq!(included, vec![1]);
    }

    #[test]
    fn rejects_images_over_pixel_limit_before_decode() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&4_001u32.to_be_bytes());
        bytes.extend_from_slice(&4_000u32.to_be_bytes());
        assert!(validate_downloaded_image(
            bytes,
            "image/png".to_string(),
            "https://example.com/large.png".to_string(),
        )
        .is_none());
    }

    #[test]
    fn image_pixel_limit_is_inclusive() {
        assert!(image_dimensions_allowed(4_000, 4_000));
        assert!(!image_dimensions_allowed(4_001, 4_000));
        assert!(!image_dimensions_allowed(0, 4_000));
        assert_eq!(IMAGE_DECODER_MAX_ALLOC, 64 * 1024 * 1024);
    }

    #[test]
    fn configured_download_size_is_capped_at_fifty_mib() {
        assert_eq!(configured_max_download_bytes(500.0), 50 * 1024 * 1024);
        assert_eq!(configured_max_download_bytes(f64::NAN), 1024 * 1024 / 10);
    }

    #[test]
    fn duplicate_hashes_keep_candidate_order() {
        let mut later = stored(PathBuf::from("later"), 2);
        later.sha256 = "same".to_string();
        let mut earlier = stored(PathBuf::from("earlier"), 1);
        earlier.sha256 = "same".to_string();

        let deduped = dedupe_downloaded(vec![(1, later), (0, earlier)]);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].local_path, PathBuf::from("earlier"));
    }

    #[tokio::test]
    async fn publish_preserves_preexisting_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let call_dir = CallTempDir::new(dir.path()).unwrap();
        let staged = call_dir.path().join("candidate.png");
        write_temp_file(&staged, b"existing").await.unwrap();
        let mut item = stored(staged, 1);
        item.size_bytes = b"existing".len();
        item.sha256 = hex::encode(Sha256::digest(b"existing"));
        let final_path = dir.path().join(format!("webimg-{}.png", item.sha256));
        tokio::fs::write(&final_path, b"existing").await.unwrap();

        publish_image(dir.path(), &mut item).await.unwrap();

        assert_eq!(item.local_path, final_path);
        assert_eq!(tokio::fs::read(final_path).await.unwrap(), b"existing");
    }

    #[tokio::test]
    async fn concurrent_same_hash_publishes_one_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let first_dir = CallTempDir::new(dir.path()).unwrap();
        let second_dir = CallTempDir::new(dir.path()).unwrap();
        let first_path = first_dir.path().join("first.png");
        let second_path = second_dir.path().join("second.png");
        write_temp_file(&first_path, b"complete").await.unwrap();
        write_temp_file(&second_path, b"complete").await.unwrap();
        let mut first = stored(first_path, 1);
        let mut second = stored(second_path, 2);
        first.size_bytes = b"complete".len();
        second.size_bytes = b"complete".len();
        first.sha256 = hex::encode(Sha256::digest(b"complete"));
        second.sha256 = first.sha256.clone();

        let (first_result, second_result) = tokio::join!(
            publish_image(dir.path(), &mut first),
            publish_image(dir.path(), &mut second)
        );
        first_result.unwrap();
        second_result.unwrap();

        assert_eq!(first.local_path, second.local_path);
        assert_eq!(
            tokio::fs::read(&first.local_path).await.unwrap(),
            b"complete"
        );
        drop(first_dir);
        drop(second_dir);
        assert_eq!(
            tokio::fs::read(&first.local_path).await.unwrap(),
            b"complete"
        );
    }

    #[tokio::test]
    async fn publish_repairs_truncated_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let call_dir = CallTempDir::new(dir.path()).unwrap();
        let staged = call_dir.path().join("candidate.png");
        write_temp_file(&staged, b"complete").await.unwrap();
        let mut item = stored(staged, 1);
        item.size_bytes = b"complete".len();
        item.sha256 = hex::encode(Sha256::digest(b"complete"));
        let final_path = dir.path().join(format!("webimg-{}.png", item.sha256));
        tokio::fs::write(&final_path, b"cut").await.unwrap();

        publish_image(dir.path(), &mut item).await.unwrap();

        assert_eq!(tokio::fs::read(final_path).await.unwrap(), b"complete");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn publish_replaces_invalid_symlink_but_not_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let call_dir = CallTempDir::new(dir.path()).unwrap();
        let staged = call_dir.path().join("candidate.png");
        write_temp_file(&staged, b"complete").await.unwrap();
        let mut item = stored(staged, 1);
        item.size_bytes = b"complete".len();
        item.sha256 = hex::encode(Sha256::digest(b"complete"));
        let final_path = dir.path().join(format!("webimg-{}.png", item.sha256));
        let target = dir.path().join("outside");
        tokio::fs::write(&target, b"outside").await.unwrap();
        symlink(&target, &final_path).unwrap();

        publish_image(dir.path(), &mut item).await.unwrap();
        assert_eq!(tokio::fs::read(&final_path).await.unwrap(), b"complete");

        let directory_hash = hex::encode(Sha256::digest(b"directory"));
        let directory_path = dir.path().join(format!("webimg-{directory_hash}.png"));
        tokio::fs::create_dir(&directory_path).await.unwrap();
        let directory_staged = call_dir.path().join("directory.png");
        write_temp_file(&directory_staged, b"complete")
            .await
            .unwrap();
        let mut directory_item = stored(directory_staged, 2);
        directory_item.size_bytes = b"complete".len();
        directory_item.sha256 = directory_hash;
        let error = publish_image(dir.path(), &mut directory_item)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cache path is a directory"));
        assert!(directory_path.is_dir());
    }

    #[tokio::test]
    async fn abort_cleans_call_temp_directory() {
        let cache = tempfile::tempdir().unwrap();
        let cache_path = cache.path().to_path_buf();
        let (path_sender, path_receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let call_dir = CallTempDir::new(&cache_path).unwrap();
            let staged = call_dir.path().join("candidate.png");
            write_temp_file(&staged, b"temporary").await.unwrap();
            path_sender.send(call_dir.path().to_path_buf()).unwrap();
            futures_util::future::pending::<()>().await;
            drop(call_dir);
        });
        let call_path = path_receiver.await.unwrap();
        assert!(call_path.exists());

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        assert!(!call_path.exists());
    }

    #[tokio::test]
    #[ignore = "live network smoke test"]
    async fn live_provider_smoke_test() {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let mut successes = 0;
        for provider in [
            ImageSearchProvider::DuckDuckGo,
            ImageSearchProvider::BingCn,
            ImageSearchProvider::Baidu,
            ImageSearchProvider::So360,
        ] {
            let result =
                search_with_provider(&client, provider, "", "杭州西湖 断桥残雪 实景", 8, true)
                    .await;
            if result.as_ref().is_ok_and(|items| !items.is_empty()) {
                successes += 1;
            } else {
                eprintln!("{}: {result:?}", provider.id());
            }
        }
        assert!(successes >= 3, "only {successes} providers succeeded");
    }

    #[tokio::test]
    #[ignore = "live network smoke test"]
    async fn live_pinned_download_smoke_test() {
        let (bytes, _, mime) = download_image_bytes(
            "https://www.rust-lang.org/logos/rust-logo-512x512.png",
            "https://www.rust-lang.org/",
            2 * 1024 * 1024,
            Instant::now() + Duration::from_secs(20),
        )
        .await
        .unwrap();
        assert_eq!(
            detect_image_mime(&bytes, &mime, ""),
            Some("image/png".to_string())
        );
    }
}
