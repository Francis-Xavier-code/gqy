//! tests — 自 src/platforms/plugins/message_history/store.rs 外移。
#![cfg(test)]

use super::*;

#[cfg(test)]
use tempfile::TempDir;

fn group(account: &str, group_id: &str) -> GroupKey {
    GroupKey::new("onebot", account, group_id).unwrap()
}

fn private(account: &str, user_id: &str) -> ConversationKey {
    ConversationKey::for_kind("onebot", account, ConversationKind::Private, user_id).unwrap()
}

fn message(
    group: GroupKey,
    message_id: impl Into<String>,
    sender_id: &str,
    sender_name: &str,
    text: impl Into<String>,
    sent_at: i64,
) -> NewHistoryMessage {
    NewHistoryMessage {
        group,
        message_id: message_id.into(),
        sender_id: sender_id.to_string(),
        sender_name: sender_name.to_string(),
        content: SanitizedContent::new(text, Vec::new()),
        reply_to_message_id: None,
        is_bot: false,
        sent_at,
        ingress_order: None,
    }
}

fn test_store() -> (TempDir, HistoryStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = HistoryStore::new(temp.path().join("nested/group_history.db"));
    (temp, store)
}

#[tokio::test]
async fn database_is_lazy_and_uses_bounded_sqlite_settings() {
    let (_temp, store) = test_store();
    assert!(!store.db_path().exists());

    assert!(store
        .recent(RecentQuery::for_context(group("1", "10"), "default", 20))
        .await
        .unwrap()
        .messages
        .is_empty());
    assert!(store.db_path().exists());

    let conn = Connection::open(store.db_path()).unwrap();
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let auto_vacuum: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(auto_vacuum, 2);
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn version_one_database_migrates_with_nullable_ingress_order() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages (
             id INTEGER PRIMARY KEY,
             platform TEXT NOT NULL,
             account_id TEXT NOT NULL,
             group_id TEXT NOT NULL,
             message_id TEXT NOT NULL,
             sender_id TEXT NOT NULL,
             sender_name TEXT NOT NULL,
             text TEXT NOT NULL,
             media_json TEXT NOT NULL,
             mentions_json TEXT NOT NULL,
             reply_to_message_id TEXT,
             is_bot INTEGER NOT NULL,
             sent_at INTEGER NOT NULL,
             recalled_at INTEGER,
             recorded_at INTEGER NOT NULL DEFAULT (unixepoch()),
             UNIQUE (platform, account_id, group_id, message_id)
         );
         PRAGMA user_version = 1;",
    )
    .unwrap();

    migrate(&conn).unwrap();

    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let has_ingress_order = conn
        .prepare("PRAGMA table_info(messages)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .any(|column| column == "ingress_order");
    let has_conversation_kind = conn
        .prepare("PRAGMA table_info(messages)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .any(|column| column == "conversation_kind");
    assert_eq!(version, SCHEMA_VERSION);
    assert!(has_ingress_order);
    assert!(has_conversation_kind);
}

#[tokio::test]
async fn private_and_group_conversations_are_isolated_and_filterable() {
    let (_temp, store) = test_store();
    let private_key = private("bot", "42");
    let group_key = group("bot", "42");
    store
        .record_message(message(
            private_key.clone(),
            "same-id",
            "42",
            "Alice",
            "private first",
            10,
        ))
        .await
        .unwrap();
    store
        .record_message(message(
            private_key.clone(),
            "private-2",
            "7",
            "Bob",
            "private second",
            20,
        ))
        .await
        .unwrap();
    store
        .record_message(message(
            group_key.clone(),
            "same-id",
            "42",
            "Alice",
            "group message",
            15,
        ))
        .await
        .unwrap();

    let private_page = store
        .search(SearchQuery::new(
            HistoryScope::Private(private_key.clone()),
            "private",
            20,
        ))
        .await
        .unwrap();
    assert_eq!(private_page.messages.len(), 2);
    assert!(private_page
        .messages
        .iter()
        .all(|message| message.group == private_key));

    let account_page = store
        .search(SearchQuery::new(
            HistoryScope::Account(private_key.account_scope()),
            "",
            20,
        ))
        .await
        .unwrap();
    assert_eq!(account_page.messages.len(), 3);
    assert!(account_page
        .messages
        .iter()
        .any(|message| message.group == group_key));
    assert!(account_page
        .messages
        .iter()
        .any(|message| message.group == private_key));

    let mut request = DeleteRequest::all(HistoryScope::Private(private_key.clone()), 30);
    request.sender_id = Some("42".to_string());
    request.since = Some(10);
    request.until = Some(10);
    let report = store.delete_history(request).await.unwrap();
    assert_eq!(report.messages_deleted, 1);
    let remaining_private = store
        .recent(RecentQuery::for_history(private_key, 20))
        .await
        .unwrap();
    assert_eq!(remaining_private.messages.len(), 1);
    assert_eq!(remaining_private.messages[0].message_id, "private-2");
    assert_eq!(
        store
            .recent(RecentQuery::for_history(group_key, 20))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
}

#[tokio::test]
async fn records_are_idempotent_isolated_and_sanitized() {
    let (_temp, store) = test_store();
    let first_group = group("bot-a", "group-1");
    let other_group = group("bot-a", "group-2");
    let other_account = group("bot-b", "group-1");
    let mut first = message(
        first_group.clone(),
        "m1",
        "u1",
        "Alice\nAdmin",
        " hello\0 world ",
        10,
    );
    first.content.media = vec![
        MediaPlaceholder::new(MediaKind::Image, Some(" cat\nphoto "), Some(" image/png ")),
        MediaPlaceholder::new(MediaKind::File, Some("notes.txt"), None::<String>),
    ];
    first.content.mentioned_user_ids = vec!["u2".to_string(), "u2".to_string()];
    first.content.mentioned_users = vec![PlatformMention {
        user_id: "u2".to_string(),
        display_name: Some("Yu\nyi".to_string()),
    }];

    let outcome = store.record_message(first.clone()).await.unwrap();
    assert!(outcome.inserted);
    let duplicate = store.record_message(first).await.unwrap();
    assert!(!duplicate.inserted);
    assert_eq!(outcome.row_id, duplicate.row_id);
    store
        .record_message(message(
            other_group.clone(),
            "m1",
            "u2",
            "Bob",
            "other group",
            11,
        ))
        .await
        .unwrap();
    store
        .record_message(message(
            other_account.clone(),
            "m1",
            "u3",
            "Carol",
            "other account",
            12,
        ))
        .await
        .unwrap();

    let page = store
        .recent(RecentQuery::for_history(first_group, 20))
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    let stored = &page.messages[0];
    assert_eq!(stored.sender_name, "Alice Admin");
    assert_eq!(stored.content.text, "hello world");
    assert_eq!(stored.content.media[0].label.as_deref(), Some("cat photo"));
    assert_eq!(stored.content.media[0].mime.as_deref(), Some("image/png"));
    assert_eq!(stored.content.mentioned_user_ids, vec!["u2"]);
    assert_eq!(stored.content.mentioned_users[0].user_id, "u2");
    assert_eq!(
        stored.content.mentioned_users[0].display_name.as_deref(),
        Some("Yu yi")
    );
    assert_eq!(stored.group.group_id(), "group-1");
}

#[tokio::test]
async fn the_reply_window_can_start_after_what_a_previous_turn_already_showed() {
    let (_temp, store) = test_store();
    let key = group("bot-a", "group-1");
    let mut first = message(key.clone(), "m1", "u1", "One", "已经发过", 10);
    first.ingress_order = Some(100);
    let mut second = message(key.clone(), "m2", "u2", "Two", "也发过", 10);
    second.ingress_order = Some(200);
    let mut third = message(key.clone(), "m3", "u3", "Three", "新到的", 10);
    third.ingress_order = Some(300);
    store
        .record_messages(vec![first, second, third])
        .await
        .unwrap();

    // Everything up to the watermark is already sitting in the replayed
    // conversation history, so the turn only carries what arrived since.
    let page = store
        .recent(RecentQuery::for_context(key.clone(), "default", 20).after_ingress_order(Some(200)))
        .await
        .unwrap();
    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["m3"]
    );

    // No watermark yet — the first turn of a conversation still gets a full
    // opening snapshot.
    let page = store
        .recent(RecentQuery::for_context(key, "default", 20))
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 3);
}

#[tokio::test]
async fn context_ingress_boundary_excludes_current_and_future_messages() {
    let (_temp, store) = test_store();
    let key = group("bot-a", "group-1");
    let mut future = message(key.clone(), "future", "u3", "Future", "future", 10);
    future.ingress_order = Some(300);
    let mut previous = message(key.clone(), "previous", "u1", "Previous", "previous", 10);
    previous.ingress_order = Some(100);
    let mut current = message(key.clone(), "current", "u2", "Current", "current", 10);
    current.ingress_order = Some(200);

    // Deliberately persist in transport-opposite order to reproduce an
    // earlier message waiting on async metadata while a later one records.
    store
        .record_messages(vec![future, previous, current])
        .await
        .unwrap();

    let page = store
        .recent(RecentQuery::for_context(key, "default", 20).before_ingress_order(Some(200)))
        .await
        .unwrap();
    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["previous"]
    );
}

#[tokio::test]
async fn context_history_is_ordered_by_transport_ingress() {
    let (_temp, store) = test_store();
    let key = group("bot-a", "group-1");
    let mut first = message(key.clone(), "first", "u1", "First", "first", 30);
    first.ingress_order = Some(100);
    let mut second = message(key.clone(), "second", "u2", "Second", "second", 10);
    second.ingress_order = Some(200);
    let mut third = message(key.clone(), "third", "u3", "Third", "third", 20);
    third.ingress_order = Some(300);

    store
        .record_messages(vec![third, first, second])
        .await
        .unwrap();

    let page = store
        .recent(RecentQuery::for_context(key, "default", 20).before_ingress_order(Some(400)))
        .await
        .unwrap();
    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second", "third"]
    );
}

#[tokio::test]
async fn reset_boundary_only_changes_automatic_context() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    store
        .record_messages(vec![
            message(key.clone(), "m1", "u", "A", "before one", 10),
            message(key.clone(), "m2", "u", "A", "before two", 20),
        ])
        .await
        .unwrap();
    let boundary = store
        .reset_context(key.clone(), "default".to_string(), 25)
        .await
        .unwrap();
    assert_eq!(boundary.after_row_id, 2);
    store
        .record_message(message(key.clone(), "m3", "u", "A", "after reset", 30))
        .await
        .unwrap();

    let context = store
        .recent(RecentQuery::for_context(key.clone(), "default", 20))
        .await
        .unwrap();
    assert_eq!(
        context
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["m3"]
    );
    let history = store
        .recent(RecentQuery::for_history(key, 20))
        .await
        .unwrap();
    assert_eq!(history.messages.len(), 3);
    let other_persona = store
        .recent(RecentQuery::for_context(group("bot", "group"), "other", 20))
        .await
        .unwrap();
    assert_eq!(other_persona.messages.len(), 3);
}

#[tokio::test]
async fn recall_before_or_after_message_is_applied_and_hidden() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    let early = store
        .record_recall(NewRecall {
            group: key.clone(),
            message_id: "early".to_string(),
            operator_id: Some("moderator".to_string()),
            recalled_at: 12,
        })
        .await
        .unwrap();
    assert!(early.newly_recorded);
    assert!(!early.matched_message);
    store
        .record_messages(vec![
            message(key.clone(), "early", "u1", "A", "hidden early", 10),
            message(key.clone(), "late", "u2", "B", "hidden late", 20),
            message(key.clone(), "visible", "u3", "C", "visible", 30),
        ])
        .await
        .unwrap();
    let late = store
        .record_recall(NewRecall {
            group: key.clone(),
            message_id: "late".to_string(),
            operator_id: None,
            recalled_at: 22,
        })
        .await
        .unwrap();
    assert!(late.matched_message);

    let visible = store
        .recent(RecentQuery::for_history(key.clone(), 20))
        .await
        .unwrap();
    assert_eq!(visible.messages.len(), 1);
    assert_eq!(visible.messages[0].message_id, "visible");

    let mut with_recalls = RecentQuery::for_history(key, 20);
    with_recalls.include_recalled = true;
    let page = store.recent(with_recalls).await.unwrap();
    assert_eq!(page.messages.len(), 3);
    assert_eq!(page.messages[0].recalled_at, Some(12));
    assert_eq!(page.messages[1].recalled_at, Some(22));
}

#[tokio::test]
async fn activity_ranking_is_scoped_stable_and_counts_recalled_messages() {
    let (_temp, store) = test_store();
    let key = group("bot-a", "group-1");
    let other_group = group("bot-a", "group-2");
    let other_account = group("bot-b", "group-1");
    let first_day = SECONDS_PER_DAY * 10 + 43_200;
    let second_day = first_day + SECONDS_PER_DAY * 2;
    let mut bot_one = message(
        key.clone(),
        "bot-1",
        "bot-alias-1",
        "GQY old",
        "bot",
        second_day + 20,
    );
    bot_one.is_bot = true;
    let mut bot_two = message(
        key.clone(),
        "bot-2",
        "bot-alias-2",
        "GQY",
        "bot",
        second_day + 30,
    );
    bot_two.is_bot = true;
    store
        .record_messages(vec![
            message(key.clone(), "a-1", "1", "Alice old", "one", first_day),
            message(key.clone(), "a-2", "1", "Alice", "two", second_day + 10),
            message(
                key.clone(),
                "a-3",
                "1",
                "Alice newest",
                "three",
                second_day + 40,
            ),
            message(key.clone(), "b-1", "2", "Bob", "one", first_day + 10),
            message(key.clone(), "b-2", "2", "Bob", "two", second_day + 20),
            bot_one,
            bot_two,
            message(
                other_group,
                "other-group",
                "3",
                "Other",
                "ignored",
                second_day,
            ),
            message(
                other_account,
                "other-account",
                "4",
                "Other",
                "ignored",
                second_day,
            ),
        ])
        .await
        .unwrap();
    store
        .record_recall(NewRecall {
            group: key.clone(),
            message_id: "a-1".to_string(),
            operator_id: Some("1".to_string()),
            recalled_at: first_day + 100,
        })
        .await
        .unwrap();

    let ranking = store
        .activity_ranking(ActivityRankingQuery {
            group: key.clone(),
            since: first_day,
            until: second_day + 100,
            limit: 2,
            include_bot: true,
        })
        .await
        .unwrap();
    assert_eq!(ranking.total_messages, 7);
    assert_eq!(ranking.participant_count, 3);
    assert_eq!(ranking.items.len(), 2);
    assert_eq!(ranking.items[0].sender_id, "1");
    assert_eq!(ranking.items[0].sender_name, "Alice newest");
    assert_eq!(ranking.items[0].message_count, 3);
    assert_eq!(ranking.items[0].active_days, 2);
    assert_eq!(ranking.items[1].sender_id, "bot-a");
    assert_eq!(ranking.items[1].sender_name, "GQY");
    assert_eq!(ranking.items[1].rank, 2);

    let without_bot = store
        .activity_ranking(ActivityRankingQuery {
            group: key,
            since: first_day,
            until: second_day + 100,
            limit: usize::MAX,
            include_bot: false,
        })
        .await
        .unwrap();
    assert_eq!(without_bot.total_messages, 5);
    assert_eq!(without_bot.participant_count, 2);
    assert_eq!(without_bot.items.len(), 2);
    assert_eq!(without_bot.items[1].sender_id, "2");
}

#[tokio::test]
async fn activity_ranking_validates_time_range_and_includes_both_boundaries() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    store
        .record_messages(vec![
            message(key.clone(), "before", "1", "A", "before", 9),
            message(key.clone(), "start", "1", "A", "start", 10),
            message(key.clone(), "end", "2", "B", "end", 20),
            message(key.clone(), "after", "2", "B", "after", 21),
        ])
        .await
        .unwrap();

    let result = store
        .activity_ranking(ActivityRankingQuery {
            group: key.clone(),
            since: 10,
            until: 20,
            limit: 20,
            include_bot: true,
        })
        .await
        .unwrap();
    assert_eq!(result.total_messages, 2);
    assert_eq!(result.participant_count, 2);
    assert!(store
        .activity_ranking(ActivityRankingQuery {
            group: key,
            since: 20,
            until: 10,
            limit: 20,
            include_bot: true,
        })
        .await
        .is_err());
}

#[tokio::test]
async fn fts_search_is_safe_paginated_and_capped_at_one_thousand() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    for batch_start in (0..1_005).step_by(MAX_BATCH_MESSAGES) {
        let end = (batch_start + MAX_BATCH_MESSAGES).min(1_005);
        let batch = (batch_start..end)
            .map(|index| {
                message(
                    key.clone(),
                    format!("m{index}"),
                    "u",
                    "Search User",
                    format!("needle item {index}"),
                    index as i64,
                )
            })
            .collect();
        store.record_messages(batch).await.unwrap();
    }
    store
        .record_message(message(
            key.clone(),
            "chinese",
            "u",
            "中文用户",
            "今天天气很好",
            1_000,
        ))
        .await
        .unwrap();

    let first = store
        .search(SearchQuery::new(
            HistoryScope::Group(key.clone()),
            "needle",
            usize::MAX,
        ))
        .await
        .unwrap();
    assert_eq!(first.messages.len(), MAX_PAGE_SIZE);
    assert!(first.next_cursor.is_some());
    let mut second_query =
        SearchQuery::new(HistoryScope::Group(key.clone()), "needle", MAX_PAGE_SIZE);
    second_query.before = first.next_cursor;
    let second = store.search(second_query).await.unwrap();
    assert_eq!(second.messages.len(), 5);
    assert!(second.next_cursor.is_none());

    let quoted = store
        .search(SearchQuery::new(
            HistoryScope::Group(key.clone()),
            "needle \"item\"",
            10,
        ))
        .await;
    assert!(quoted.is_ok());

    let chinese_trigram = store
        .search(SearchQuery::new(
            HistoryScope::Group(key.clone()),
            "天气很",
            10,
        ))
        .await
        .unwrap();
    assert_eq!(chinese_trigram.messages[0].message_id, "chinese");
    let chinese_short_fallback = store
        .search(SearchQuery::new(HistoryScope::Group(key), "天气", 10))
        .await
        .unwrap();
    assert_eq!(chinese_short_fallback.messages[0].message_id, "chinese");
}

#[tokio::test]
async fn search_can_filter_recent_messages_by_sender_id() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    store
        .record_messages(vec![
            message(key.clone(), "a1", "10001", "A", "first", 1),
            message(key.clone(), "b1", "10002", "B", "other", 2),
            message(key.clone(), "a2", "10001", "A", "second", 3),
            message(key.clone(), "a3", "10001", "A", "third", 4),
        ])
        .await
        .unwrap();

    let mut query = SearchQuery::new(HistoryScope::Group(key), "", 10);
    query.sender_id = Some("10001".to_string());
    let page = store.search(query).await.unwrap();

    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a3", "a2", "a1"]
    );
}

#[tokio::test]
async fn history_pages_are_limited_by_message_count_only() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    let large_text = format!("needle {}", "x".repeat(60 * 1024));
    let messages = (0..10)
        .map(|index| {
            message(
                key.clone(),
                format!("large-{index}"),
                "u",
                "Search User",
                large_text.clone(),
                index,
            )
        })
        .collect();
    store.record_messages(messages).await.unwrap();

    let page = store
        .search(SearchQuery::new(
            HistoryScope::Group(key),
            "needle",
            MAX_PAGE_SIZE,
        ))
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 10);
    assert!(page.next_cursor.is_none());
}

#[tokio::test]
async fn explicit_deletion_is_batched_and_does_not_cross_scope() {
    let (_temp, store) = test_store();
    let first = group("bot", "first");
    let second = group("bot", "second");
    let other_account = group("other-bot", "first");
    let day = SECONDS_PER_DAY;
    store
        .record_messages(vec![
            message(first.clone(), "old1", "u", "A", "old one", day),
            message(first.clone(), "old2", "u", "A", "old two", day * 2),
            message(first.clone(), "new", "u", "A", "new", day * 9),
            message(second.clone(), "same-account", "u", "A", "keep", day),
            message(
                other_account.clone(),
                "other-account",
                "u",
                "A",
                "keep",
                day,
            ),
        ])
        .await
        .unwrap();
    store
        .reset_context(first.clone(), "default".to_string(), day * 2)
        .await
        .unwrap();
    store
        .record_recall(NewRecall {
            group: first.clone(),
            message_id: "old1".to_string(),
            operator_id: None,
            recalled_at: day * 2,
        })
        .await
        .unwrap();

    let mut request =
        DeleteRequest::keep_days(HistoryScope::Group(first.clone()), 3, day * 10).unwrap();
    request.batch_size = 1;
    let report = store.delete_history(request).await.unwrap();
    assert_eq!(report.messages_deleted, 2);
    assert_eq!(report.recalls_deleted, 1);
    assert_eq!(report.boundaries_deleted, 1);
    assert!(report.batches >= 3);

    let first_page = store
        .recent(RecentQuery::for_history(first.clone(), 20))
        .await
        .unwrap();
    assert_eq!(first_page.messages.len(), 1);
    assert_eq!(first_page.messages[0].message_id, "new");
    assert!(store
        .context_boundary(first.clone(), "default".to_string())
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .recent(RecentQuery::for_history(second.clone(), 20))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    assert_eq!(
        store
            .recent(RecentQuery::for_history(other_account.clone(), 20))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );

    let all = store
        .delete_history(DeleteRequest::all(
            HistoryScope::Group(first.clone()),
            day * 10,
        ))
        .await
        .unwrap();
    assert_eq!(all.messages_deleted, 1);
    assert!(store
        .recent(RecentQuery::for_history(first, 20))
        .await
        .unwrap()
        .messages
        .is_empty());

    let account_scope = HistoryScope::Account(second.account_scope());
    let account_search = store
        .search(SearchQuery::new(account_scope.clone(), "keep", 20))
        .await
        .unwrap();
    assert_eq!(account_search.messages.len(), 1);
    assert_eq!(account_search.messages[0].group, second);
    let account_report = store
        .delete_history(DeleteRequest::all(account_scope, day * 10))
        .await
        .unwrap();
    assert_eq!(account_report.messages_deleted, 1);
    assert_eq!(
        store
            .recent(RecentQuery::for_history(other_account, 20))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
}

#[tokio::test]
async fn retained_reset_boundary_does_not_hide_reused_rowids_after_cleanup() {
    let (_temp, store) = test_store();
    let key = group("bot", "group");
    let day = SECONDS_PER_DAY;

    store
        .record_message(message(key.clone(), "before-reset", "u", "A", "old", day))
        .await
        .unwrap();
    let boundary = store
        .reset_context(key.clone(), "default".to_string(), day * 10)
        .await
        .unwrap();
    assert_eq!(boundary.after_row_id, 1);

    // The message is outside the retention window, while the reset itself
    // is recent enough to remain. Deleting the sole message lets SQLite
    // reuse rowid 1 for the next insert.
    store
        .delete_history(
            DeleteRequest::keep_days(HistoryScope::Group(key.clone()), 3, day * 10).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .context_boundary(key.clone(), "default".to_string())
            .await
            .unwrap()
            .unwrap()
            .after_row_id,
        0
    );

    let inserted = store
        .record_message(message(
            key.clone(),
            "after-cleanup",
            "u",
            "A",
            "new",
            day * 10,
        ))
        .await
        .unwrap();
    assert_eq!(inserted.row_id, 1);
    let context = store
        .recent(RecentQuery::for_context(key, "default", 20))
        .await
        .unwrap();
    assert_eq!(
        context
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["after-cleanup"]
    );
}

#[test]
fn opening_an_existing_database_repairs_a_stale_reset_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("history.db");
    let key = group("bot", "group");
    {
        let conn = open_database(&path).unwrap();
        conn.execute(
            "INSERT INTO context_boundaries (
                 platform, account_id, conversation_kind, conversation_id,
                 persona_scope, after_row_id, reset_at
             ) VALUES (?1, ?2, ?3, ?4, 'default', 99, 123)",
            params![
                key.platform(),
                key.account_id(),
                key.conversation_kind(),
                key.conversation_id()
            ],
        )
        .unwrap();
    }

    let conn = open_database(&path).unwrap();
    assert_eq!(
        read_boundary(&conn, &key, "default")
            .unwrap()
            .unwrap()
            .after_row_id,
        0
    );
}

#[test]
fn identifiers_and_keep_days_are_validated() {
    assert!(GroupKey::new("onebot", "", "group").is_err());
    assert!(GroupKey::new("onebot", "bot", "bad\ngroup").is_err());
    let scope = HistoryScope::Account(AccountKey::new("onebot", "bot").unwrap());
    assert!(DeleteRequest::keep_days(scope, 0, 0).is_err());
}
