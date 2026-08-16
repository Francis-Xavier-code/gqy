//! tests — 自 src/platforms/plugins/real_context/affection.rs 外移。
#![cfg(test)]

use super::*;

#[test]
fn default_score_has_no_active_reply_bias() {
    let settings = RealContextPluginSettings::default();
    assert_eq!(
        reply_bias(&settings, settings.affection_initial_score, "1"),
        0.0
    );
    assert!(reply_bias(&settings, settings.affection_min_score, "1") < 0.0);
    assert!(reply_bias(&settings, settings.affection_regular_max_score, "1") > 0.0);
}

#[test]
fn ordinary_users_stop_before_the_close_level() {
    let settings = RealContextPluginSettings::default();
    assert_eq!(
        clamp_score(&settings, 100.0, "1"),
        settings.affection_regular_max_score
    );
    assert_eq!(level_for_score(&settings, 100.0, "1").name, "信任");
}

#[test]
fn relationship_query_requires_user_id() {
    assert!(required_user_id(&json!({})).is_err());
    assert!(required_user_id(&json!({ "user_id": "  " })).is_err());
    assert!(required_user_id(&json!({ "user_id": 123 })).is_err());
    assert_eq!(
        required_user_id(&json!({ "user_id": " QQ:2606945861 " })).unwrap(),
        "2606945861"
    );
}

#[test]
fn affection_profile_keys_are_isolated_by_persona() {
    let default = AppConfig::default();
    let mut custom = default.clone();
    custom.prompt.active_persona = "Group Persona.md".to_string();

    assert_ne!(profile_key(&default), profile_key(&custom));
    assert!(profile_key(&default).starts_with(LEGACY_PROFILE_KEY));
}

#[test]
fn model_tags_are_bounded_and_event_tags_are_rejected() {
    let tags = clean_tags(
        vec![
            " 技术求助 ".to_string(),
            "Rust 用户".to_string(),
            "Rust 用户".to_string(),
            "x".repeat(40),
        ],
        2,
    );
    assert_eq!(tags[0], "Rust 用户");
    assert_eq!(tags.len(), 2);
    assert!(tags[1].chars().count() <= MAX_TAG_CHARS);
}

#[test]
fn removing_and_readding_the_same_tag_is_not_a_change() {
    let previous = vec!["Rust 用户".to_string()];
    let (added, removed) = tag_changes(&previous, &["Rust 用户".to_string()]);
    assert!(added.is_empty());
    assert!(removed.is_empty());
}

#[test]
fn affection_logs_are_bilingual_and_keep_operational_ids() {
    let chinese = format_affection_initialized_log(
        "3927564101",
        "130515298",
        "Shiroha_xyz",
        "3888705871",
        "default",
        10.0,
        "中立",
        Locale::Zh,
    );
    assert!(chinese.starts_with("【好感度：初始化】\n"));
    assert!(chinese.contains("用户：Shiroha_xyz（QQ 3888705871）"));
    assert!(chinese.contains("初始分数：10.000"));

    let english = format_affection_initialized_log(
        "3927564101",
        "130515298",
        "Shiroha_xyz",
        "3888705871",
        "default",
        10.0,
        "中立",
        Locale::En,
    );
    assert!(english.starts_with("[Affection: initialized]\n"));
    assert!(english.contains("Initial relationship: neutral"));
    assert!(english.contains("Initial score: 10.000"));

    let skipped = format_affection_skipped_log(
        "3927564101",
        "130515298",
        "Shiroha_xyz",
        "3888705871",
        "low_confidence",
        Some(0.42),
        Some(0.70),
        Locale::Zh,
    );
    assert!(skipped.contains("原因：置信度不足"));
    assert!(skipped.contains("置信度：0.42"));
    assert!(skipped.contains("阈值：0.70"));

    let failed = format_affection_failure_log(
        "3927564101",
        "130515298",
        "Shiroha_xyz",
        "3888705871",
        "model_call",
        "request\ntimeout",
        Locale::En,
    );
    assert!(failed.starts_with("[Affection: update failed]\n"));
    assert!(failed.contains("Error: request timeout"));
}
