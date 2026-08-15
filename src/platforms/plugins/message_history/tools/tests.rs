//! tests — 自 src/platforms/plugins/message_history/tools.rs 外移。
#![cfg(test)]

use super::*;

use super::*;
use crate::config::AppConfig;
use crate::paths::GQYPaths;
use crate::platforms::plugins::PlatformPluginRegistry;
use crate::platforms::{OutboundMessage, PlatformAdapter, PlatformConversation, SendReceipt};
use crate::state::StateStore;
use futures_util::future::BoxFuture;
use std::path::PathBuf;

struct NullAdapter;

impl PlatformAdapter for NullAdapter {
    fn send<'a>(&'a self, _message: OutboundMessage) -> BoxFuture<'a, Result<SendReceipt>> {
        Box::pin(async { Ok(SendReceipt::default()) })
    }

    fn bot_display_name<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Ok("GQY".to_string()) })
    }
}

fn test_paths(root: &std::path::Path) -> GQYPaths {
    GQYPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("fish"),
        bash_hook_file: root.join("bash"),
        zsh_hook_file: root.join("zsh"),
        scripts_dir: root.join("scripts"),
        system_scripts_dir: PathBuf::new(),
    }
}

fn test_context(root: &std::path::Path, is_admin: bool) -> PlatformTurnContext {
    let paths = test_paths(root);
    let mut config = AppConfig::default();
    if is_admin {
        config.platforms.qq.admin_users.push(42);
    }
    PlatformTurnContext::new(
        PlatformConversation {
            platform: ONEBOT_PLATFORM.to_string(),
            account_id: "10000".to_string(),
            kind: ConversationKind::Private,
            conversation_id: "42".to_string(),
        },
        "42".to_string(),
        "Alice".to_string(),
        is_admin,
        config,
        paths.clone(),
        StateStore::new(&paths).unwrap(),
        Arc::new(NullAdapter),
        Arc::new(PlatformPluginRegistry::new(Vec::new())),
    )
}

fn principal(sender_id: &str) -> DeletePrincipal {
    DeletePrincipal {
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        sender_id: sender_id.to_string(),
        conversation_scope: "onebot:10000:group:42".to_string(),
    }
}

#[test]
fn ordinary_users_are_limited_to_the_current_conversation() {
    let temp = tempfile::tempdir().unwrap();
    let ordinary = test_context(temp.path(), false);
    assert!(matches!(
        history_scope(&json!({}), &ordinary, true).unwrap(),
        HistoryScope::Private(_)
    ));
    assert!(history_scope(
        &json!({ "conversation_kind": "group", "conversation_id": "99" }),
        &ordinary,
        true,
    )
    .is_err());
    assert!(history_scope(&json!({ "all_conversations": true }), &ordinary, true).is_err());

    let admin = test_context(temp.path(), true);
    assert!(matches!(
        history_scope(
            &json!({ "conversation_kind": "group", "conversation_id": "99" }),
            &admin,
            true,
        )
        .unwrap(),
        HistoryScope::Group(_)
    ));
    assert!(matches!(
        history_scope(&json!({ "all_conversations": true }), &admin, true).unwrap(),
        HistoryScope::Account(_)
    ));
}

#[test]
fn zero_history_limit_uses_the_bounded_page_maximum() {
    assert_eq!(limit(&json!({}), 0, 500), 500);
    assert_eq!(limit(&json!({ "limit": 25 }), 0, 500), 25);
    assert_eq!(limit(&json!({ "limit": 100 }), 40, 500), 40);
    assert_eq!(limit(&json!({ "limit": 2_000 }), 0, 2_000), 1_000);
}

#[test]
fn required_history_id_rejects_missing_and_invalid_values() {
    assert!(required_id(&json!({}), "user_id").is_err());
    assert!(required_id(&json!({ "user_id": "" }), "user_id").is_err());
    assert!(required_id(&json!({ "user_id": "abc" }), "user_id").is_err());
    assert_eq!(
        required_id(&json!({ "user_id": "2606945861" }), "user_id").unwrap(),
        "2606945861"
    );
}

#[test]
fn activity_ranking_times_support_original_and_rfc3339_formats() {
    assert_eq!(parse_time("1700000000", false).unwrap(), 1_700_000_000);
    assert_eq!(
        parse_time("2024-01-02T03:04:05+08:00", false).unwrap(),
        1_704_135_845
    );
    let start = parse_time("2024-01-02", false).unwrap();
    let end = parse_time("2024-01-02", true).unwrap();
    assert_eq!(end - start, 86_399);
    assert!(parse_time("2024/01/02", false).is_err());
}

#[test]
fn activity_ranking_integer_arguments_are_strict() {
    assert_eq!(
        optional_i64(&json!({ "days": -1 }), "days").unwrap(),
        Some(-1)
    );
    assert!(optional_i64(&json!({ "days": 1.5 }), "days").is_err());
    assert!(optional_string(&json!({ "start_time": 123 }), "start_time").is_err());
}

#[test]
fn group_member_search_requires_explicit_query_and_limit() {
    assert!(group_member_query(&json!({})).is_err());
    assert!(group_member_query(&json!({ "query": "  " })).is_err());
    assert_eq!(
        group_member_query(&json!({ "query": " 张三 " })).unwrap(),
        "张三"
    );

    assert!(group_member_limit(&json!({}), 20).is_err());
    assert!(group_member_limit(&json!({ "limit": 0 }), 20).is_err());
    assert!(group_member_limit(&json!({ "limit": 21 }), 20).is_err());
    assert_eq!(group_member_limit(&json!({ "limit": 20 }), 20).unwrap(), 20);
}

#[test]
fn group_member_search_matches_ids_cards_and_nicknames_by_relevance() {
    let member = PlatformGroupMember {
        group_id: "42".to_string(),
        user_id: "123456789".to_string(),
        nickname: "Alice Example".to_string(),
        card: "测试名片".to_string(),
        role: "member".to_string(),
        title: String::new(),
        joined_at: 0,
        last_active_at: 0,
    };

    assert_eq!(
        group_member_match_rank(&member, "123456789", "123456789"),
        Some(0)
    );
    assert_eq!(group_member_match_rank(&member, "3456", "3456"), Some(2));
    assert_eq!(group_member_match_rank(&member, "alice", "alice"), Some(1));
    assert_eq!(group_member_match_rank(&member, "名片", "名片"), Some(2));
    assert_eq!(group_member_match_rank(&member, "title", "title"), None);
}

fn delete_request() -> DeleteRequest {
    DeleteRequest::all(
        HistoryScope::Group(GroupKey::new("onebot", "10000", "42").unwrap()),
        1_700_000_000,
    )
}

#[test]
fn history_delete_requires_a_new_exact_message_from_the_same_admin() {
    let confirmations = DeleteConfirmations::default();
    let admin = principal("7");
    let challenge = confirmations.issue(
        admin.clone(),
        delete_request(),
        "request-message".to_string(),
    );

    assert!(confirmations
        .take_confirmed(
            &challenge.confirmation_token,
            &admin,
            "confirmation-message",
            "请确认删除这些历史",
        )
        .is_err());
    assert!(confirmations
        .take_confirmed(
            &challenge.confirmation_token,
            &admin,
            "request-message",
            &challenge.confirmation_phrase,
        )
        .is_err());
    assert!(confirmations
        .take_confirmed(
            &challenge.confirmation_token,
            &principal("8"),
            "confirmation-message",
            &challenge.confirmation_phrase,
        )
        .is_err());

    let mut other_conversation = admin.clone();
    other_conversation.conversation_scope = "onebot:10000:private:7".to_string();
    assert!(confirmations
        .take_confirmed(
            &challenge.confirmation_token,
            &other_conversation,
            "confirmation-message",
            &challenge.confirmation_phrase,
        )
        .is_err());

    let request = confirmations
        .take_confirmed(
            &challenge.confirmation_token,
            &admin,
            "confirmation-message",
            &challenge.confirmation_phrase,
        )
        .unwrap();
    assert!(matches!(request.mode, DeleteMode::All));
    assert!(confirmations
        .take_confirmed(
            &challenge.confirmation_token,
            &admin,
            "another-message",
            &challenge.confirmation_phrase,
        )
        .is_err());
}

#[test]
fn newer_delete_request_invalidates_the_same_admins_old_token() {
    let confirmations = DeleteConfirmations::default();
    let admin = principal("7");
    let old = confirmations.issue(admin.clone(), delete_request(), "old-request".to_string());
    let new = confirmations.issue(admin.clone(), delete_request(), "new-request".to_string());

    assert!(confirmations
        .take_confirmed(
            &old.confirmation_token,
            &admin,
            "confirmation",
            &old.confirmation_phrase,
        )
        .is_err());
    assert!(confirmations
        .take_confirmed(
            &new.confirmation_token,
            &admin,
            "confirmation",
            &new.confirmation_phrase,
        )
        .is_ok());
}

#[test]
fn expired_delete_confirmation_cannot_be_consumed() {
    let confirmations = DeleteConfirmations::default();
    let admin = principal("7");
    let challenge = confirmations.issue(
        admin.clone(),
        delete_request(),
        "request-message".to_string(),
    );
    confirmations
        .pending
        .lock()
        .unwrap()
        .get_mut(&challenge.confirmation_token)
        .unwrap()
        .expires_at = Instant::now();

    assert!(confirmations
        .take_confirmed(
            &challenge.confirmation_token,
            &admin,
            "confirmation-message",
            &challenge.confirmation_phrase,
        )
        .is_err());
}
