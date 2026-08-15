//! tests — 自 src/platforms/plugins/group_management.rs 外移。
#![cfg(test)]

use super::*;

use super::*;

    #[test]
    fn target_parser_accepts_only_qq_sized_numeric_ids() {
        assert_eq!(
            split_ids("12345, 12345 @678901 and invalid-12"),
            vec!["12345", "12345", "678901"]
        );
        assert!(valid_id("12345"));
        assert!(!valid_id("1234"));
        assert!(!valid_id("12a45"));
    }

    #[test]
    fn mute_duration_is_spelled_in_seconds_and_reads_back_in_words() {
        assert_eq!(humanize_seconds(0), "解禁");
        assert_eq!(humanize_seconds(600), "10分钟");
        assert_eq!(humanize_seconds(3_600), "1小时");
        // The exact case that shipped as 24 minutes: 24h must be 86400, and a
        // 1440 that a model meant as "minutes" must read back as 24 minutes so
        // the mistake is visible in the result.
        assert_eq!(humanize_seconds(86_400), "1天");
        assert_eq!(humanize_seconds(1_440), "24分钟");
        assert_eq!(humanize_seconds(MAX_BAN_SECONDS), "30天");
        assert_eq!(humanize_seconds(90), "1分钟30秒");
    }

    #[test]
    fn kick_targets_accept_an_array_and_still_fall_back_to_a_scalar() {
        // The array form is what the schema now advertises; the scalar and its
        // space/comma splitting stay for single targets and older habits.
        let ids = |value: &Value| -> Vec<String> {
            value
                .get("user_ids")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .flat_map(split_ids)
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            ids(&json!({ "user_ids": ["12345", "678901"] })),
            vec!["12345".to_string(), "678901".to_string()]
        );
        assert_eq!(split_ids("12345 678901"), vec!["12345", "678901"]);
    }

    #[test]
    fn batch_results_tell_the_model_which_targets_may_be_retried() {
        let aggregate = aggregate_target_results(vec![
            external_operation_result(json!({ "record": { "user_id": "12345" } }), Vec::new()),
            failure_for_target(anyhow::anyhow!("目标不在当前群中"), "678901"),
        ]);
        assert_eq!(aggregate["success_count"], 1);
        assert_eq!(aggregate["failed_count"], 1);
        // Mixed outcome: the successes must never be retried, the failure may
        // be retried on its own. Without this the model re-kicked the same
        // dead target over and over.
        assert_eq!(aggregate["do_not_retry"], false);
        assert_eq!(aggregate["do_not_retry_successful_targets"], true);
        assert_eq!(aggregate["retry_failed_targets_only"], true);
        assert_eq!(aggregate["failed_target_ids"], json!(["678901"]));

        let all_good = aggregate_target_results(vec![external_operation_result(
            json!({ "record": { "user_id": "12345" } }),
            Vec::new(),
        )]);
        assert_eq!(all_good["do_not_retry"], true);
    }

    fn event(action: &str, user: &str, at: i64, duration: u64, reason: &str) -> ManagementEvent {
        ManagementEvent {
            record_id: format!("{action}-{user}-{at}"),
            action: action.to_string(),
            user_id: user.to_string(),
            user_name: format!("用户{user}"),
            duration,
            happened_at: at,
            operator_id: "10000".to_string(),
            reason: reason.to_string(),
            source: "llm_tool".to_string(),
            detail: String::new(),
        }
    }

    #[test]
    fn action_filter_groups_related_event_kinds() {
        assert!(action_matches("ban", "unban"));
        assert!(action_matches("kick", "kick_black"));
        assert!(action_matches("title", "title_clear"));
        assert!(action_matches("all", "ban"));
        assert!(!action_matches("ban", "kick"));
        assert!(!action_matches("bogus", "ban"));
    }

    #[test]
    fn ban_status_reflects_later_unban_override_and_expiry() {
        let now = 1_000_000;
        let events = vec![
            event("ban", "11111", now - 100, 3_600, "刷屏"), // 后被解禁
            event("unban", "11111", now - 50, 0, ""),
            event("ban", "22222", now - 100, 600, "口嗨"), // 后被再次禁言覆盖
            event("ban", "22222", now - 50, 3_600, "加重"), // 仍在禁言期
            event("ban", "33333", now - 7_200, 600, "已过期"),
        ];
        let statuses = ban_statuses(&events, now);
        assert_eq!(statuses[&events[0].record_id], "unmuted");
        assert_eq!(statuses[&events[2].record_id], "overridden");
        assert_eq!(statuses[&events[3].record_id], "active");
        assert_eq!(statuses[&events[4].record_id], "expired");
    }

    #[test]
    fn member_stats_aggregate_counts_durations_and_last_reason() {
        let events = vec![
            event("ban", "11111", 100, 600, "刷屏"),
            event("ban", "11111", 200, 1_200, "再犯"),
            event("unban", "11111", 300, 0, ""), // 解禁不计次
            event("kick", "11111", 400, 0, "屡教不改"),
            event("kick_black", "22222", 500, 0, ""),
            event("title_set", "22222", 600, 0, ""),
        ];
        let mut stats = aggregate_member_stats("all", &events);
        stats.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].ban_count, 2);
        assert_eq!(stats[0].total_ban_duration, 1_800);
        assert_eq!(stats[0].kick_count, 1);
        assert_eq!(stats[0].last_reason, "屡教不改");
        assert_eq!(stats[0].last_action_at, 400);
        assert_eq!(stats[1].kick_count, 1);
        assert_eq!(stats[1].title_count, 1);
        assert_eq!(stats[1].ban_count, 0);

        // action 过滤只统计对应类别
        let ban_only = aggregate_member_stats("ban", &events);
        assert!(ban_only.iter().all(|item| item.kick_count == 0));
    }

    #[test]
    fn astrbot_defaults_are_preserved() {
        let settings = Settings::default();
        assert_eq!(settings.default_duration_seconds, 600);
        assert_eq!(settings.max_reason_length, 500);
        assert_eq!(settings.max_records_per_group, 500);
    }

    #[test]
    fn response_contract_uses_success_and_message() {
        let response: Value =
            serde_json::from_str(&json_result(true, "ok", json!({ "record_id": "abc" })).unwrap())
                .unwrap();
        assert_eq!(response["success"], true);
        assert_eq!(response["message"], "ok");
    }

    #[test]
    fn audit_failure_reports_partial_success_and_forbids_retry() {
        let response = external_operation_result(
            json!({ "record_id": "abc" }),
            vec!["injected audit failure".to_string()],
        );
        assert_eq!(response["success"], true);
        assert_eq!(response["operation_succeeded"], true);
        assert_eq!(response["audit_succeeded"], false);
        assert_eq!(response["do_not_retry"], true);
        assert!(response["message"].as_str().unwrap().contains("请勿重试"));
    }

    #[test]
    fn external_failure_remains_retryable() {
        let response = failure(anyhow::anyhow!("injected external failure"));
        assert_eq!(response["success"], false);
        assert_eq!(response["operation_succeeded"], false);
        assert_eq!(response["do_not_retry"], false);
    }
