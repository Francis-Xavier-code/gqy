use super::{ToolRegistry, ToolSpec};
use crate::config::AppConfig;
use crate::i18n::agent_text as t;
use crate::paths::MiyuPaths;
use crate::skills::{self, SkillEntry, SkillScope};
use anyhow::{Context, Result};
use serde_json::{json, Value};

pub fn register_skills(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
) -> Result<()> {
    let (entries, fingerprint) = stable_catalog(config, paths)?;
    register_load_skill(registry, config.clone(), paths.clone(), &entries);
    registry.set_skill_catalog_fingerprint(fingerprint);
    Ok(())
}

pub fn refresh_skills(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
) -> Result<bool> {
    if !registry.contains("load_skill") {
        return Ok(false);
    }
    let Some(snapshot) =
        prepare_skill_refresh(registry.skill_catalog_fingerprint(), config, paths)?
    else {
        return Ok(false);
    };
    apply_skill_refresh(registry, config, paths, snapshot);
    Ok(true)
}

pub(crate) struct SkillCatalogSnapshot {
    entries: Vec<SkillEntry>,
    fingerprint: [u8; 32],
}

pub(crate) fn prepare_skill_refresh(
    current: Option<[u8; 32]>,
    config: &AppConfig,
    paths: &MiyuPaths,
) -> Result<Option<SkillCatalogSnapshot>> {
    let fingerprint = skills::catalog_fingerprint(config, paths)?;
    if current == Some(fingerprint) {
        return Ok(None);
    }
    let (entries, fingerprint) = stable_catalog(config, paths)?;
    Ok(Some(SkillCatalogSnapshot {
        entries,
        fingerprint,
    }))
}

pub(crate) fn apply_skill_refresh(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
    snapshot: SkillCatalogSnapshot,
) {
    register_load_skill(registry, config.clone(), paths.clone(), &snapshot.entries);
    registry.set_skill_catalog_fingerprint(snapshot.fingerprint);
}

fn stable_catalog(config: &AppConfig, paths: &MiyuPaths) -> Result<(Vec<SkillEntry>, [u8; 32])> {
    for _ in 0..3 {
        let before = skills::catalog_fingerprint(config, paths)?;
        let entries = skills::discover(config, paths)?;
        let after = skills::catalog_fingerprint(config, paths)?;
        if before == after {
            return Ok((entries, after));
        }
    }
    anyhow::bail!("skill catalog kept changing while it was being refreshed")
}

pub fn register_authoring(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    register_create_skill(registry, config.clone(), paths.clone());
    register_update_skill(registry, config.clone(), paths.clone());
    register_delete_skill(registry, config, paths.clone());
    register_publish_skill(registry, paths.clone());
    register_list_skill_drafts(registry, paths);
}

fn register_load_skill(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    entries: &[SkillEntry],
) {
    let description = format!(
        "{}\n\n{}\n\n{}",
        t(
            "Load a specialized skill's full instructions and resources into the conversation. The skill name must match one of the available skills listed below.",
            "加载指定技能的完整指令和资源到当前对话。技能名称必须匹配下方列出的可用技能之一。",
        ),
        t(
            "Use this tool before applying a skill or using any scripts/resources from that skill. Skill allowed-tools metadata never grants Miyu permissions.",
            "应用 skill 或使用其中的脚本/资源前，必须先加载该 skill。Skill 的 allowed-tools 元数据不会授予 Miyu 权限。",
        ),
        available_skills_xml(entries),
    );
    registry.register(ToolSpec::new(
        "load_skill",
        description,
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": t("The exact skill name from the available skills list.", "可用技能列表中的准确名称。")
                }
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            let paths = paths.clone();
            async move {
                tokio::task::spawn_blocking(move || load_skill(args, &config, &paths))
                    .await
                    .context("skill loader worker stopped")?
            }
        },
    ));
}

fn register_create_skill(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    registry.register(
        ToolSpec::new(
            "create_skill",
            t(
                "Create a hidden draft for a new Miyu skill. Use the returned absolute skill_dir and skill_file with apply_patch, then call publish_skill. This never overwrites an existing skill.",
                "为新的 Miyu skill 创建隐藏草稿。使用返回的绝对 skill_dir 和 skill_file 配合 apply_patch 编辑，随后调用 publish_skill。此操作不会覆盖已有 skill。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "pattern": "^[a-z0-9]+(-[a-z0-9]+)*$",
                        "description": t("Skill name, which must follow the Agent Skills naming rules.", "Skill 名称，必须符合 Agent Skills 命名规则。")
                    },
                    "description": {
                        "type": "string",
                        "description": t("What the skill does and when it should be used.", "Skill 做什么以及何时应使用。")
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["global", "persona"],
                        "default": "global",
                        "description": t("global is available to every persona; persona belongs to the current persona.", "global 对所有人格可用；persona 仅属于当前人格。")
                    }
                },
                "required": ["name", "description"],
                "additionalProperties": false
            }),
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move {
                    tokio::task::spawn_blocking(move || create_skill(args, &config, &paths))
                        .await
                        .context("skill draft worker stopped")?
                }
            },
        )
        .writes(),
    );
}

fn register_update_skill(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    registry.register(
        ToolSpec::new(
            "update_skill",
            t(
                "Create an isolated update draft copied from an existing skill. Edit only the returned draft, then call publish_skill. Publishing fails if the live skill changed meanwhile.",
                "从已有 skill 复制一个隔离的更新草稿。只编辑返回的草稿，然后调用 publish_skill；若 live skill 同期发生变化，发布会失败。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": t("Existing skill name.", "已有 skill 名称。") },
                    "scope": {
                        "type": "string",
                        "enum": ["global", "persona"],
                        "description": t("Exact scope containing the skill.", "包含该 skill 的准确作用域。")
                    }
                },
                "required": ["name", "scope"],
                "additionalProperties": false
            }),
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move {
                    tokio::task::spawn_blocking(move || update_skill(args, &config, &paths))
                        .await
                        .context("skill update worker stopped")?
                }
            },
        )
        .writes(),
    );
}

fn register_delete_skill(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    registry.register(
        ToolSpec::new(
            "delete_skill",
            t(
                "Permanently delete an existing user skill from the exact global or current-persona scope.",
                "从准确的 global 或当前 persona 作用域永久删除已有用户 Skill。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": t("Existing skill name.", "已有 Skill 名称。") },
                    "scope": {
                        "type": "string",
                        "enum": ["global", "persona"],
                        "description": t("Exact scope containing the skill.", "包含该 Skill 的准确作用域。")
                    }
                },
                "required": ["name", "scope"],
                "additionalProperties": false
            }),
            move |args| {
                let config = config.clone();
                let paths = paths.clone();
                async move {
                    tokio::task::spawn_blocking(move || delete_skill(args, &config, &paths))
                        .await
                        .context("skill deletion worker stopped")?
                }
            },
        )
        .writes(),
    );
}

fn register_publish_skill(registry: &mut ToolRegistry, paths: MiyuPaths) {
    registry.register(
        ToolSpec::new(
            "publish_skill",
            t(
                "Validate and atomically publish a Miyu skill draft. Create drafts never overwrite; update drafts use revision checks. Scripts remain resources and are not registered as tools.",
                "校验并原子发布 Miyu skill 草稿。创建草稿绝不覆盖；更新草稿执行版本检查。scripts 仍是资源，不会注册为工具。",
            ),
            json!({
                "type": "object",
                "properties": {
                    "draft_id": { "type": "string", "description": t("Draft ID returned by create_skill or update_skill.", "create_skill 或 update_skill 返回的草稿 ID。") }
                },
                "required": ["draft_id"],
                "additionalProperties": false
            }),
            move |args| {
                let paths = paths.clone();
                async move {
                    tokio::task::spawn_blocking(move || publish_skill(args, &paths))
                        .await
                        .context("skill publish worker stopped")?
                }
            },
        )
        .writes(),
    );
}

fn register_list_skill_drafts(registry: &mut ToolRegistry, paths: MiyuPaths) {
    registry.register(
        ToolSpec::new(
            "list_skill_drafts",
            t(
                "List retained Miyu skill drafts. Drafts with no changes for 30 days are removed before listing.",
                "列出保留的 Miyu skill 草稿。列出前会清理 30 天未修改的草稿。",
            ),
            json!({"type":"object","properties":{},"additionalProperties":false}),
            move |_| {
                let paths = paths.clone();
                async move {
                    tokio::task::spawn_blocking(move || {
                        Ok(serde_json::to_string_pretty(&json!({
                            "ok": true,
                            "drafts": skills::list_drafts(&paths)?,
                        }))?)
                    })
                    .await
                    .context("skill draft listing worker stopped")?
                }
            },
        )
        .writes(),
    );
}

fn load_skill(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let name = required_string(&args, "name")?;
    let loaded = skills::load(&name, config, paths)?;
    let base_dir = loaded
        .base_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "built-in".to_string());
    let files = if loaded.files.is_empty() {
        String::new()
    } else {
        format!(
            "\n<skill_files>\n{}\n</skill_files>",
            loaded
                .files
                .iter()
                .map(|path| format!("  <file>{}</file>", xml_escape(&path.display().to_string())))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let metadata = skill_metadata_xml(&loaded.metadata);
    Ok(format!(
        "<skill_content name=\"{}\" source=\"{}\">\n{}\n<skill_instructions format=\"markdown\">\n{}\n</skill_instructions>\n\n<skill_base_dir>{}</skill_base_dir>{}\n</skill_content>",
        xml_escape(&loaded.metadata.name),
        loaded.source.as_str(),
        metadata,
        xml_escape(&loaded.body),
        xml_escape(&base_dir),
        files,
    ))
}

fn skill_metadata_xml(metadata: &crate::skills::SkillMetadata) -> String {
    let mut fields = vec![format!(
        "  <description>{}</description>",
        xml_escape(&metadata.description)
    )];
    if let Some(license) = &metadata.license {
        fields.push(format!("  <license>{}</license>", xml_escape(license)));
    }
    if let Some(compatibility) = &metadata.compatibility {
        fields.push(format!(
            "  <compatibility>{}</compatibility>",
            xml_escape(compatibility)
        ));
    }
    if let Some(allowed_tools) = &metadata.allowed_tools {
        fields.push(format!(
            "  <allowed_tools grants_permissions=\"false\">{}</allowed_tools>",
            xml_escape(allowed_tools)
        ));
    }
    for (key, value) in &metadata.metadata {
        fields.push(format!(
            "  <entry key=\"{}\">{}</entry>",
            xml_escape(key),
            xml_escape(value)
        ));
    }
    format!("<skill_metadata>\n{}\n</skill_metadata>", fields.join("\n"))
}

fn create_skill(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let name = required_string(&args, "name")?;
    let description = required_string(&args, "description")?;
    let scope = SkillScope::parse(args.get("scope").and_then(Value::as_str))?;
    let draft = skills::create_draft(config, paths, &name, &description, scope)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "state": "draft",
        "draft": draft,
        "next": "Edit only the returned draft with apply_patch, then call publish_skill with draft_id."
    }))?)
}

fn update_skill(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let name = required_string(&args, "name")?;
    let scope_value = required_string(&args, "scope")?;
    let scope = SkillScope::parse(Some(&scope_value))?;
    let draft = skills::update_draft(config, paths, &name, scope)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "state": "draft",
        "draft": draft,
        "next": "Edit only the returned draft with apply_patch, then call publish_skill with draft_id."
    }))?)
}

fn publish_skill(args: Value, paths: &MiyuPaths) -> Result<String> {
    let draft_id = required_string(&args, "draft_id")?;
    let published = skills::publish_draft(paths, &draft_id)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "state": "published",
        "skill": published,
        "catalog_refresh": "next tool round",
    }))?)
}

fn delete_skill(args: Value, config: &AppConfig, paths: &MiyuPaths) -> Result<String> {
    let name = required_string(&args, "name")?;
    let scope_value = required_string(&args, "scope")?;
    let scope = SkillScope::parse(Some(&scope_value))?;
    let deleted = skills::delete_skill(config, paths, &name, scope)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "state": "deleted",
        "skill": deleted,
        "catalog_refresh": "next tool round",
    }))?)
}

fn required_string(args: &Value, key: &str) -> Result<String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        anyhow::bail!("{key} is required");
    }
    Ok(value.to_string())
}

fn available_skills_xml(entries: &[SkillEntry]) -> String {
    let items = entries
        .iter()
        .map(|entry| {
            format!(
                "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n    <source>{}</source>\n  </skill>",
                xml_escape(&entry.metadata.name),
                xml_escape(&entry.metadata.description),
                entry.source.as_str(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("<available_skills>\n{items}\n</available_skills>")
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_paths(root: &std::path::Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("data/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("data/pictures"),
            fish_hook_file: root.join("fish/miyu.fish"),
            bash_hook_file: root.join("config/shell/bash-hook.sh"),
            zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
            scripts_dir: root.join("data/scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }

    #[test]
    fn load_skill_description_includes_builtin_creator() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let mut registry = ToolRegistry::new();
        register_skills(&mut registry, &config, &paths).unwrap();
        let description = &registry.get("load_skill").unwrap().description;
        assert!(description.contains("<name>skill-creator</name>"));
        assert!(description.contains("<source>built_in</source>"));
    }

    #[test]
    fn loaded_skill_exposes_standard_metadata_without_granting_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let directory = paths.skills_dir.join("sample-skill");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            "---\nname: sample-skill\ndescription: Sample workflow\nlicense: MIT\ncompatibility: Miyu\nallowed-tools: run_command\nmetadata:\n  author: test\n---\n\nBody.",
        )
        .unwrap();

        let loaded = load_skill(
            json!({"name": "sample-skill"}),
            &AppConfig::default(),
            &paths,
        )
        .unwrap();
        assert!(loaded.contains("<license>MIT</license>"));
        assert!(loaded.contains("<compatibility>Miyu</compatibility>"));
        assert!(loaded
            .contains("<allowed_tools grants_permissions=\"false\">run_command</allowed_tools>"));
        assert!(loaded.contains("<entry key=\"author\">test</entry>"));
    }

    #[test]
    fn authoring_tools_have_write_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut registry = ToolRegistry::new();
        register_authoring(&mut registry, AppConfig::default(), paths);
        for name in [
            "create_skill",
            "update_skill",
            "delete_skill",
            "publish_skill",
        ] {
            assert_eq!(
                registry.permission(name).unwrap(),
                super::super::ToolPermission::Writes
            );
        }
        assert_eq!(
            registry.permission("list_skill_drafts").unwrap(),
            super::super::ToolPermission::Writes
        );
    }

    #[test]
    fn refresh_detects_new_skill_once_without_a_watcher() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let mut registry = ToolRegistry::new();
        register_skills(&mut registry, &config, &paths).unwrap();
        let directory = paths.skills_dir.join("new-skill");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            "---\nname: new-skill\ndescription: Newly added skill\n---\n",
        )
        .unwrap();

        assert!(refresh_skills(&mut registry, &config, &paths).unwrap());
        assert!(registry
            .get("load_skill")
            .unwrap()
            .description
            .contains("<name>new-skill</name>"));
        assert!(!refresh_skills(&mut registry, &config, &paths).unwrap());
    }

    #[test]
    fn dynamic_load_skill_keeps_the_builtin_loading_policy() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        let mut registry = ToolRegistry::new();
        register_skills(&mut registry, &config, &paths).unwrap();
        let load_skill = registry.get("load_skill").unwrap();
        assert!(!load_skill.always_loaded);
        assert_eq!(
            load_skill.load_policy,
            super::super::tool_descriptions::LoadPolicy::Summary
        );
        assert_eq!(load_skill.groups, vec!["skills"]);
    }

    #[test]
    fn update_skill_requires_an_explicit_scope_at_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let error = update_skill(
            json!({"name": "sample-skill"}),
            &AppConfig::default(),
            &paths,
        )
        .unwrap_err();
        assert!(error.to_string().contains("scope is required"));
    }

    #[test]
    fn delete_skill_requires_an_explicit_scope_at_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let error = delete_skill(
            json!({"name": "sample-skill"}),
            &AppConfig::default(),
            &paths,
        )
        .unwrap_err();
        assert!(error.to_string().contains("scope is required"));
    }
}
