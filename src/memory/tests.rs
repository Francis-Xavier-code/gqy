//! tests — 自 src/memory/mod.rs 外移。
#![cfg(test)]

use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::paths::GQYPaths;

    fn test_paths(temp: &tempfile::TempDir) -> GQYPaths {
        GQYPaths {
            root_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            config_file: temp.path().join("config/config.jsonc"),
            skills_dir: temp.path().join("config/skills"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
            state_dir: temp.path().join("state"),
            pictures_dir: temp.path().join("pictures"),
            fish_hook_file: temp.path().join("fish/gqy.fish"),
            bash_hook_file: temp.path().join("shell/bash-hook.sh"),
            zsh_hook_file: temp.path().join("shell/zsh-hook.zsh"),
            scripts_dir: temp.path().join("config/scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }

    #[test]
    fn evicted_search_is_indexed_and_can_be_narrowed_by_time() {
        let temp = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(&AppConfig::default(), &test_paths(&temp));
        store.init().unwrap();
        let rows: Vec<EvictedTurn> = (0..1200)
            .map(|index| EvictedTurn {
                source_id: format!("t{index}:user"),
                timestamp: format!("2026-08-{:02}T10:00:00+00:00", (index % 28) + 1),
                role: "user".to_string(),
                content: format!("第 {index} 轮，聊到了 蓝色小刺猬 这个话题"),
                ..EvictedTurn::default()
            })
            .collect();
        store.remember_evicted_turns(&rows).unwrap();

        // The scan used to stop at the newest 1000 rows, so anything older was
        // stored forever and reachable never.
        let oldest = store
            .search_evicted_context_readonly("第 3 轮", 50, None, None)
            .unwrap();
        assert!(
            oldest["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|hit| hit["snippet"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("第 3 轮")),
            "{oldest}"
        );

        // "What were we talking about that morning" is a question about when.
        let ranged = store
            .search_evicted_context_readonly(
                "蓝色小刺猬",
                50,
                Some("2026-08-05T00:00:00+00:00"),
                Some("2026-08-05T23:59:59+00:00"),
            )
            .unwrap();
        let hits = ranged["results"].as_array().unwrap();
        assert!(!hits.is_empty(), "{ranged}");
        assert!(
            hits.iter().all(|hit| hit["timestamp"]
                .as_str()
                .unwrap_or_default()
                .starts_with("2026-08-05")),
            "{ranged}"
        );
    }

    fn diary_config(batch_size: usize) -> AppConfig {
        let mut config = AppConfig::default();
        config.plugins.memory.diary_batch_size = batch_size;
        config
    }

    fn test_origin() -> MemoryOrigin {
        MemoryOrigin::local("test-session")
    }

    fn platform_origin(user_id: &str, display_name: &str) -> MemoryOrigin {
        MemoryOrigin {
            kind: "platform".to_string(),
            platform: "onebot".to_string(),
            account_id: "10000".to_string(),
            conversation_kind: "private".to_string(),
            conversation_id: user_id.to_string(),
            sender_id: user_id.to_string(),
            sender_display_name: display_name.to_string(),
            session_id: format!("session-{user_id}"),
            message_id: format!("message-{user_id}"),
        }
    }

    fn scoped_store(
        config: &AppConfig,
        paths: &GQYPaths,
        origin: &MemoryOrigin,
        privileged: bool,
    ) -> MemoryStore {
        let ownership = origin.principal_ownership().unwrap();
        MemoryStore::new(config, paths).with_request_context(
            if privileged {
                MemoryAccess::Privileged
            } else {
                MemoryAccess::principal(ownership.owner_principal.clone())
            },
            Some(ownership.owner_principal),
            ownership.owner_display_name,
        )
    }

    #[test]
    fn compact_jieba_matches_reference_segmentation() {
        let reference = jieba_rs::Jieba::new();
        for input in [
            "我们中出了一个叛徒",
            "macOS 输入法需要切换中英文",
            "访达窗口规则和中文输入法配置",
            "podman-compose 不能直接重新创建容器",
            "北京烤鸭真好吃，后天天气不好。",
            "Rust 2024 edition与C++20",
        ] {
            assert_eq!(
                JIEBA.cut(input),
                reference.cut(input, false),
                "segmentation differs for {input}"
            );
        }
    }

    fn record_turn(store: &MemoryStore, user: &str, assistant: &str) -> bool {
        let (database_id, generation) = store.identity().unwrap();
        store
            .process_after_turn(user, assistant, &test_origin(), &database_id, generation)
            .unwrap()
    }

    #[test]
    fn remembers_and_recalls_fact() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
            .unwrap();
        let result = store.recall_memories("Niri XMODIFIERS", 5, false).unwrap();
        assert!(result.to_string().contains("XMODIFIERS"));
    }

    #[test]
    fn ordinary_principals_recall_only_public_and_owned_memories() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let admin = MemoryStore::new(&config, &paths);
        admin.init().unwrap();
        let timestamp = now();
        admin
            .data_conn()
            .unwrap()
            .execute(
                "INSERT INTO facts (
                    content, source, status, confidence, recall_count, created_at, updated_at,
                    visibility, owner_principal, owner_display_name
                 ) VALUES (?1, 'test', 'active', 1.0, 0, ?2, ?2, 'public', '', '')",
                params!["隔离测试 公共知识", timestamp],
            )
            .unwrap();

        let origin_a = platform_origin("7", "Alice");
        let origin_b = platform_origin("8", "Bob");
        let user_a = scoped_store(&config, &paths, &origin_a, false);
        let user_b = scoped_store(&config, &paths, &origin_b, false);
        user_a
            .remember_fact("隔离测试 Alice 私密事实", "test")
            .unwrap();
        user_b
            .remember_fact("隔离测试 Bob 私密事实", "test")
            .unwrap();
        let (database_id, generation) = user_a.identity().unwrap();
        user_a
            .process_after_turn(
                "隔离测试 Alice 的旧事件",
                "只属于 Alice",
                &origin_a,
                &database_id,
                generation,
            )
            .unwrap();

        let a = user_a
            .recall_memories("隔离测试", 20, false)
            .unwrap()
            .to_string();
        assert!(a.contains("公共知识"));
        assert!(a.contains("Alice 私密事实"));
        assert!(a.contains("Alice 的旧事件"));
        assert!(!a.contains("Bob 私密事实"));

        let b = user_b
            .recall_memories("隔离测试", 20, false)
            .unwrap()
            .to_string();
        assert!(b.contains("公共知识"));
        assert!(b.contains("Bob 私密事实"));
        assert!(!b.contains("Alice 私密事实"));
        assert!(!b.contains("Alice 的旧事件"));
        let b_events = user_b
            .recall_past_events("隔离测试", 20)
            .unwrap()
            .to_string();
        assert!(!b_events.contains("Alice 的旧事件"));
        let a_events = user_a
            .recall_past_events("隔离测试", 20)
            .unwrap()
            .to_string();
        assert!(a_events.contains("Alice 的旧事件"));

        let privileged = admin
            .recall_memories("隔离测试", 20, false)
            .unwrap()
            .to_string();
        assert!(privileged.contains("公共知识"));
        assert!(privileged.contains("Alice 私密事实"));
        assert!(privileged.contains("Bob 私密事实"));
        assert!(privileged.contains("Alice 的旧事件"));
    }

    #[test]
    fn evicted_context_uses_the_same_principal_filter() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let origin_a = platform_origin("7", "Alice");
        let origin_b = platform_origin("8", "Bob");
        let user_a = scoped_store(&config, &paths, &origin_a, false);
        let user_b = scoped_store(&config, &paths, &origin_b, false);
        user_a
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "a:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "淘汰记忆 Alice 专属".to_string(),
                ..EvictedTurn::default()
            }])
            .unwrap();
        user_b
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "b:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "淘汰记忆 Bob 专属".to_string(),
                ..EvictedTurn::default()
            }])
            .unwrap();

        let a = user_a
            .search_evicted_context("淘汰记忆", 10)
            .unwrap()
            .to_string();
        assert!(a.contains("Alice 专属"));
        assert!(!a.contains("Bob 专属"));
        let all = MemoryStore::new(&config, &paths)
            .search_evicted_context("淘汰记忆", 10)
            .unwrap()
            .to_string();
        assert!(all.contains("Alice 专属"));
        assert!(all.contains("Bob 专属"));
    }

    #[test]
    fn access_migration_backfills_platform_principals_conservatively() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        let origin = platform_origin("7", "Alice");
        let (database_id, generation) = store.identity().unwrap();
        store
            .process_after_turn(
                "迁移归属测试",
                "迁移回答",
                &origin,
                &database_id,
                generation,
            )
            .unwrap();
        let conn = store.data_conn().unwrap();
        let episode_id = conn
            .query_row("SELECT id FROM episodes LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO facts (
                content, source, status, confidence, recall_count, created_at, updated_at,
                source_episode_ids, visibility, owner_principal, owner_display_name
             ) VALUES ('迁移事实', 'test', 'active', 1.0, 0, ?1, ?1, ?2,
                       'privileged', '', '')",
            params![now(), serde_json::to_string(&vec![episode_id]).unwrap()],
        )
        .unwrap();
        conn.execute(
            "UPDATE episodes SET visibility='privileged', owner_principal='', owner_display_name=''",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE memory_meta SET access_schema_version=0 WHERE id=1",
            [],
        )
        .unwrap();
        drop(conn);

        store.init().unwrap();
        let expected = origin.principal_ownership().unwrap().owner_principal;
        let conn = store.data_conn().unwrap();
        let episode_owner = conn
            .query_row(
                "SELECT visibility, owner_principal FROM episodes WHERE id=?1",
                [episode_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        let fact_owner = conn
            .query_row(
                "SELECT visibility, owner_principal FROM facts WHERE content='迁移事实'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(
            episode_owner,
            (VISIBILITY_PRINCIPAL.to_string(), expected.clone())
        );
        assert_eq!(fact_owner, (VISIBILITY_PRINCIPAL.to_string(), expected));
    }

    #[test]
    fn organizer_can_publish_general_facts_but_cannot_update_another_principal() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(2);
        let paths = test_paths(&temp);
        let origin_a = platform_origin("7", "Alice");
        let origin_b = platform_origin("8", "Bob");
        let user_a = scoped_store(&config, &paths, &origin_a, false);
        let user_b = scoped_store(&config, &paths, &origin_b, false);
        let bob_fact = user_b
            .remember_fact("macOS 隔离主题是 Bob 的私人偏好", "test")
            .unwrap();
        let (database_id, generation) = user_b.identity().unwrap();
        user_b
            .process_after_turn(
                "macOS 隔离主题 Bob 的设置",
                "Bob 使用另一种方式",
                &origin_b,
                &database_id,
                generation,
            )
            .unwrap();
        let (database_id, generation) = user_a.identity().unwrap();
        user_a
            .process_after_turn(
                "macOS 隔离主题与通用命令",
                "使用 launchctl print",
                &origin_a,
                &database_id,
                generation,
            )
            .unwrap();
        let batch = MemoryStore::new(&config, &paths)
            .next_organization_batch()
            .unwrap()
            .unwrap();
        assert!(batch.existing.iter().any(|memory| memory.id == bob_fact));
        let alice_principal = origin_a.principal_ownership().unwrap().owner_principal;
        let source_id = batch
            .diaries
            .iter()
            .find(|diary| diary.owner_principal.as_deref() == Some(alice_principal.as_str()))
            .unwrap()
            .id;
        let cross_user_update = OrganizedOutput {
            knowledge: vec![KnowledgeAction {
                operation: "update".to_string(),
                target_id: Some(bob_fact),
                memory_type: "preference".to_string(),
                content: "macOS 隔离主题被 Alice 覆盖".to_string(),
                truth_status: "reported".to_string(),
                importance: 3,
                confidence: 0.8,
                visibility: VISIBILITY_PRINCIPAL.to_string(),
                subjects: Vec::new(),
                tags: Vec::new(),
                diary_ids: vec![source_id],
            }],
            long_diaries: Vec::new(),
        };
        assert!(MemoryStore::new(&config, &paths)
            .apply_organized_batch(&batch, cross_user_update)
            .unwrap_err()
            .to_string()
            .contains("different principal"));

        let leaky_public_fact = OrganizedOutput {
            knowledge: vec![KnowledgeAction {
                operation: "create".to_string(),
                target_id: None,
                memory_type: "fact".to_string(),
                content: "Alice 使用 macOS 的私人经历".to_string(),
                truth_status: "reported".to_string(),
                importance: 3,
                confidence: 0.8,
                visibility: VISIBILITY_PUBLIC.to_string(),
                subjects: Vec::new(),
                tags: Vec::new(),
                diary_ids: vec![source_id],
            }],
            long_diaries: Vec::new(),
        };
        assert!(MemoryStore::new(&config, &paths)
            .apply_organized_batch(&batch, leaky_public_fact)
            .unwrap_err()
            .to_string()
            .contains("source identity marker"));

        MemoryStore::new(&config, &paths)
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: vec![KnowledgeAction {
                        operation: "create".to_string(),
                        target_id: None,
                        memory_type: "fact".to_string(),
                        content: "macOS 通用知识使用 launchctl print".to_string(),
                        truth_status: "accepted".to_string(),
                        importance: 3,
                        confidence: 0.9,
                        visibility: VISIBILITY_PUBLIC.to_string(),
                        subjects: Vec::new(),
                        tags: vec!["macOS".to_string()],
                        diary_ids: vec![source_id],
                    }],
                    long_diaries: Vec::new(),
                },
            )
            .unwrap();
        let bob_recall = user_b
            .recall_memories("launchctl", 10, false)
            .unwrap()
            .to_string();
        assert!(bob_recall.contains("macOS 通用知识"));
    }

    #[test]
    fn association_excludes_own_sessions_visible_echo() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store.init().unwrap();
        store
            .data_conn()
            .unwrap()
            .execute(
                "INSERT INTO episodes (content, source, status, recall_count, created_at, updated_at, retention, origin_session_id)
                 VALUES ('对方提到自回声话题', 'auto_diary', 'active', 0, ?1, ?1, 'short_term', 's1')",
                [chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        // 无排除:能召回。
        assert!(store.association("自回声", None).unwrap().is_some());
        // 同会话且晚于最老可见轮 → 自回声被滤(原对话就在眼前)。
        let exclusion = AssociationExclusion {
            session_id: "s1".to_string(),
            since: "2000-01-01T00:00:00Z".to_string(),
        };
        assert!(store
            .association("自回声", Some(&exclusion))
            .unwrap()
            .is_none());
        // 别的会话不受排除影响。
        let other = AssociationExclusion {
            session_id: "s2".to_string(),
            since: "2000-01-01T00:00:00Z".to_string(),
        };
        assert!(store.association("自回声", Some(&other)).unwrap().is_some());
    }

    #[test]
    fn unrelated_and_rejected_memories_are_not_associated() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        let rejected = store.remember_fact("旧的错误结论", "test").unwrap();
        store
            .data_conn()
            .unwrap()
            .execute(
                "UPDATE facts SET truth_status='rejected' WHERE id=?1",
                [rejected],
            )
            .unwrap();
        assert!(store.association("完全无关的主题", None).unwrap().is_none());
        assert!(store.association("错误结论", None).unwrap().is_none());
    }

    #[test]
    fn association_format_always_keeps_its_closing_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.plugins.memory.association_max_chars = 128;
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        let hit = MemoryHit {
            id: 1,
            kind: MemoryKind::Fact,
            content: "很长的知识点".repeat(100),
            score: 1.0,
            timestamp: now(),
            source: "test".to_string(),
            retention: None,
            visibility: VISIBILITY_PUBLIC.to_string(),
            owner_principal: String::new(),
            owner_display_name: String::new(),
            subjects: "[]".to_string(),
            source_episode_ids: Vec::new(),
            origin_session_id: String::new(),
        };
        let formatted = store.format_association(&AssociationContext {
            facts: vec![hit],
            episodes: Vec::new(),
            organization_due: false,
        });
        assert!(formatted.ends_with("</associative-memory>"));
        assert!(formatted.chars().count() <= 128);
    }

    #[test]
    fn association_lines_carry_date_and_dedupe_diary_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        let stamp = now();
        let date = association_date(&stamp).unwrap();
        let base = MemoryHit {
            id: 1,
            kind: MemoryKind::Fact,
            content: "知识点内容".to_string(),
            score: 1.0,
            timestamp: stamp.clone(),
            source: "test".to_string(),
            retention: None,
            visibility: VISIBILITY_PUBLIC.to_string(),
            owner_principal: String::new(),
            owner_display_name: String::new(),
            subjects: "[]".to_string(),
            source_episode_ids: Vec::new(),
            origin_session_id: String::new(),
        };
        let diary = MemoryHit {
            id: 2,
            kind: MemoryKind::Diary,
            content: format!("{stamp}，对方说：测试；我回：通过"),
            retention: Some(SHORT_TERM.to_string()),
            ..base.clone()
        };
        let formatted = store.format_association(&AssociationContext {
            facts: vec![base],
            episodes: vec![diary],
            organization_due: false,
        });
        assert!(formatted.contains(&format!("[{date}] [公共知识] 知识点内容")));
        assert!(formatted.contains(&format!("[{date}] [公共知识] 对方说：测试；我回：通过")));
        assert!(!formatted.contains(&stamp));
    }

    #[test]
    fn diary_content_reads_as_a_first_person_exchange() {
        let content = diary_content(
            "2026-08-10T12:00:00+00:00",
            "wps 保存文件默认的编码是gbk吗",
            "分情况：纯文本默认 GBK，docx 内部是 UTF-8",
        );
        assert_eq!(
            content,
            "2026-08-10T12:00:00+00:00，对方说：wps 保存文件默认的编码是gbk吗；我回：分情况：纯文本默认 GBK，docx 内部是 UTF-8"
        );
    }

    #[test]
    fn association_dedup_filters_visible_lines_and_keeps_changed_ones() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        assert!(store.association_dedup_enabled());
        let stamp = now();
        let fact = MemoryHit {
            id: 1,
            kind: MemoryKind::Fact,
            content: "Homebrew 的 GitHub 镜像只读".to_string(),
            score: 1.0,
            timestamp: stamp.clone(),
            source: "test".to_string(),
            retention: None,
            visibility: VISIBILITY_PUBLIC.to_string(),
            owner_principal: String::new(),
            owner_display_name: String::new(),
            subjects: "[]".to_string(),
            source_episode_ids: Vec::new(),
            origin_session_id: String::new(),
        };
        let diary = MemoryHit {
            id: 2,
            kind: MemoryKind::Diary,
            content: "对方说：测试；我回：通过".to_string(),
            retention: Some(SHORT_TERM.to_string()),
            ..fact.clone()
        };
        let updated_fact = MemoryHit {
            id: 1,
            content: "Homebrew 的 GitHub 镜像只读，推送需走官方地址".to_string(),
            ..fact.clone()
        };
        // 第一回合的注入块回放时携带的行
        let first = store.format_association(&AssociationContext {
            facts: vec![fact.clone()],
            episodes: vec![diary.clone()],
            organization_due: false,
        });
        let seen = first
            .lines()
            .filter(|line| line.starts_with("- ["))
            .collect::<HashSet<_>>();
        assert_eq!(seen.len(), 2);
        // 未变化的 fact 与 diary 被过滤；内容更新过的 fact 保留
        let mut association = AssociationContext {
            facts: vec![fact.clone(), updated_fact],
            episodes: vec![diary],
            organization_due: false,
        };
        store.retain_unseen_association(&mut association, &seen);
        assert_eq!(association.facts.len(), 1);
        assert!(association.facts[0].content.contains("官方地址"));
        assert!(association.episodes.is_empty());
        // 空 seen 集不过滤
        let mut untouched = AssociationContext {
            facts: vec![fact],
            episodes: Vec::new(),
            organization_due: false,
        };
        store.retain_unseen_association(&mut untouched, &HashSet::new());
        assert_eq!(untouched.facts.len(), 1);
    }

    #[test]
    fn reset_all_clears_facts_and_episodes() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
            .unwrap();
        store.remember_pending_event("你好", "在呢").unwrap();
        store.flush_pending_events().unwrap();

        let before = store.recall_memories("你好 XMODIFIERS", 5, false).unwrap();
        assert!(!before["facts"].as_array().unwrap().is_empty());
        assert!(!before["episodes"].as_array().unwrap().is_empty());

        store.reset_all(false).unwrap();

        let after = store.recall_memories("你好 XMODIFIERS", 5, false).unwrap();
        assert!(after["facts"].as_array().unwrap().is_empty());
        assert!(after["episodes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn evicted_context_can_be_cleared() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "turn-1:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "旧上下文 输入法".to_string(),
                ..EvictedTurn::default()
            }])
            .unwrap();
        store
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "turn-1:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "旧上下文 输入法".to_string(),
                ..EvictedTurn::default()
            }])
            .unwrap();
        assert_eq!(
            store.search_evicted_context("输入法", 5).unwrap()["results"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .search_evicted_context("输入法", 5)
            .unwrap()
            .to_string()
            .contains("旧上下文"));
        store.clear_evicted_context().unwrap();
        assert!(!store
            .search_evicted_context("输入法", 5)
            .unwrap()
            .to_string()
            .contains("旧上下文"));
    }

    #[test]
    fn disabled_writes_block_content_but_allow_recall_reinforcement() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let mut store = MemoryStore::new(&config, &paths);
        let fact_id = store
            .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
            .unwrap();

        store.set_writes_enabled(false);
        assert_eq!(store.remember_fact("不应保存", "test").unwrap(), 0);
        assert!(!record_turn(&store, "不应写入日记", "不会写入"));
        assert!(store.prepare_evicted_context_db().unwrap().is_none());

        let association = store.association("Niri XMODIFIERS", None).unwrap();
        assert!(association.is_some());
        let conn = store.data_conn().unwrap();
        let recall_count = conn
            .query_row(
                "SELECT recall_count FROM facts WHERE id=?1",
                [fact_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(recall_count, 1);
        assert_eq!(count_rows(&conn, "facts").unwrap(), 1);
        assert_eq!(count_rows(&conn, "episodes").unwrap(), 0);
        assert_eq!(count_rows(&conn, "pending_events").unwrap(), 0);
    }

    #[test]
    fn diary_batch_starts_only_at_the_configured_turn_count() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(14);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        for index in 0..13 {
            assert!(record_turn(
                &store,
                &format!("问题 {index}"),
                &format!("回答 {index}")
            ));
        }
        assert!(store.next_organization_batch().unwrap().is_none());
        assert!(record_turn(&store, "第十四问", "第十四答"));
        let batch = store.next_organization_batch().unwrap().unwrap();
        assert_eq!(batch.diaries.len(), 14);
        assert_eq!(batch.diaries[0].origin.kind, "local");
        assert_eq!(batch.diaries[0].origin.session_id, "test-session");

        store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: Vec::new(),
                },
            )
            .unwrap();
        let conn = store.data_conn().unwrap();
        assert_eq!(
            count_where(
                &conn,
                "episodes",
                "retention='short_term' AND consolidated_at IS NULL"
            )
            .unwrap(),
            0
        );
        assert_eq!(
            count_where(&conn, "episodes", "retention='short_term'").unwrap(),
            14
        );
    }

    #[test]
    fn third_recall_requires_and_applies_long_diary_promotion() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(14);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        assert!(record_turn(&store, "macOS 输入法配置", "切换中英文输入法"));
        for _ in 0..3 {
            assert!(store.association("macOS 输入法", None).unwrap().is_some());
        }
        let batch = store.next_organization_batch().unwrap().unwrap();
        assert_eq!(batch.diaries.len(), 1);
        assert!(batch.diaries[0].force_long_term);
        let source_id = batch.diaries[0].id;
        store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: vec![LongDiaryDraft {
                        content: "我曾帮助处理 macOS 输入法配置。".to_string(),
                        importance: 3,
                        confidence: 0.9,
                        visibility: VISIBILITY_PRIVILEGED.to_string(),
                        subjects: Vec::new(),
                        tags: vec!["macOS".to_string(), "输入法".to_string()],
                        diary_ids: vec![source_id],
                    }],
                },
            )
            .unwrap();

        let conn = store.data_conn().unwrap();
        let (pending, promoted): (i64, Option<String>) = conn
            .query_row(
                "SELECT promotion_pending, promoted_at FROM episodes WHERE id=?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(pending, 0);
        assert!(promoted.is_some());
        assert_eq!(
            count_where(&conn, "episodes", "retention='long_term'").unwrap(),
            1
        );
    }

    #[test]
    fn reset_all_invalidates_an_inflight_organization_batch() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(2);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        assert!(record_turn(&store, "问题一", "回答一"));
        assert!(record_turn(&store, "问题二", "回答二"));
        let batch = store.next_organization_batch().unwrap().unwrap();
        let stale_database_id = batch.database_id.clone();
        let stale_generation = batch.generation;

        store.reset_all(false).unwrap();
        assert!(!store
            .process_after_turn(
                "重置前启动的问题",
                "不应写回",
                &test_origin(),
                &stale_database_id,
                stale_generation,
            )
            .unwrap());
        assert!(store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: Vec::new(),
                },
            )
            .is_err());
        let conn = store.data_conn().unwrap();
        assert_eq!(count_rows(&conn, "facts").unwrap(), 0);
        assert_eq!(count_rows(&conn, "episodes").unwrap(), 0);
    }

    #[test]
    fn cleanup_deletes_only_expired_consolidated_short_diaries() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(2);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store.init().unwrap();
        let conn = store.data_conn().unwrap();
        conn.execute(
            "INSERT INTO episodes (
                content, source, status, created_at, updated_at, retention,
                expires_at, consolidated_at
             ) VALUES ('expired', 'episode', 'active', ?1, ?1, 'short_term', ?1, ?1)",
            ["2020-01-01T00:00:00Z"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO episodes (
                content, source, status, created_at, updated_at, retention,
                expires_at, consolidated_at
             ) VALUES ('pending', 'episode', 'active', ?1, ?1, 'short_term', ?1, NULL)",
            ["2020-01-01T00:00:00Z"],
        )
        .unwrap();
        drop(conn);

        assert_eq!(store.cleanup_expired_short_diaries().unwrap(), 1);
        let conn = store.data_conn().unwrap();
        assert_eq!(count_rows(&conn, "episodes").unwrap(), 1);
        assert_eq!(
            conn.query_row("SELECT content FROM episodes", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "pending"
        );
        assert_eq!(
            conn.query_row("SELECT status FROM episodes", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "forgotten"
        );
    }

    #[test]
    fn existing_episodes_migrate_as_long_term_diaries() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        std::fs::create_dir_all(store.data_db.parent().unwrap()).unwrap();
        let conn = Connection::open(&store.data_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE episodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'episode',
                status TEXT NOT NULL DEFAULT 'active',
                strength REAL NOT NULL DEFAULT 1.0,
                recall_count INTEGER NOT NULL DEFAULT 0,
                last_recalled_at TEXT,
                last_decay_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
             );
             INSERT INTO episodes (content, created_at, updated_at)
             VALUES ('旧版长期经历', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z');",
        )
        .unwrap();
        drop(conn);

        store.init().unwrap();
        let conn = store.data_conn().unwrap();
        assert_eq!(
            conn.query_row("SELECT retention FROM episodes", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            LONG_TERM
        );
        assert_eq!(count_rows(&conn, "episodes").unwrap(), 1);
    }

    #[test]
    fn organizer_never_recreates_a_moved_persona_database() {
        let temp = tempfile::tempdir().unwrap();
        let config = diary_config(2);
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        assert!(record_turn(&store, "问题一", "回答一"));
        assert!(record_turn(&store, "问题二", "回答二"));
        let batch = store.next_organization_batch().unwrap().unwrap();
        let memory_dir = store.data_db.parent().unwrap().to_path_buf();
        let moved_dir = memory_dir.with_file_name("memory-moved");
        std::fs::rename(&memory_dir, &moved_dir).unwrap();

        assert!(store.next_organization_batch().unwrap().is_none());
        assert!(!memory_dir.exists());
        assert!(store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: Vec::new(),
                },
            )
            .is_err());
        assert!(!memory_dir.exists());

        store.init().unwrap();
        assert!(store
            .apply_organized_batch(
                &batch,
                OrganizedOutput {
                    knowledge: Vec::new(),
                    long_diaries: Vec::new(),
                },
            )
            .is_err());
    }
}
