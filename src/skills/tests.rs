//! tests — 自 src/skills.rs 外移。
#![cfg(test)]

use super::*;

use super::*;

fn test_paths(root: &Path) -> GQYPaths {
    GQYPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("data/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("data/pictures"),
        fish_hook_file: root.join("fish/gqy.fish"),
        bash_hook_file: root.join("config/shell/bash-hook.sh"),
        zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
        scripts_dir: root.join("data/scripts"),
        system_scripts_dir: PathBuf::new(),
    }
}

#[test]
fn parses_standard_frontmatter_fields() {
    let raw = "---\nname: sample-skill\ndescription: Sample workflow\nlicense: MIT\ncompatibility: GQY\nallowed-tools: read_file\nmetadata:\n  author: test\n---\n\nBody.";
    let metadata = parse_skill_metadata(raw, Some("sample-skill")).unwrap();
    assert_eq!(metadata.license.as_deref(), Some("MIT"));
    assert_eq!(metadata.compatibility.as_deref(), Some("GQY"));
    assert_eq!(metadata.allowed_tools.as_deref(), Some("read_file"));
    assert_eq!(
        metadata.metadata.get("author").map(String::as_str),
        Some("test")
    );
}

#[test]
fn rejects_yaml_anchors_before_loading_frontmatter() {
    let raw = "---\nname: sample-skill\ndescription: &description Sample workflow\nmetadata:\n  copied: *description\n---\n";
    let error = parse_skill_metadata(raw, Some("sample-skill")).unwrap_err();
    assert!(error.to_string().contains("anchors or aliases"));
}

#[test]
fn persona_skill_overrides_global_and_builtin() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let global = paths.skills_dir.join("sample-skill");
    let persona = config
        .active_persona_skills_dir(&paths)
        .join("sample-skill");
    for (directory, description) in [(&global, "global"), (&persona, "persona")] {
        fs::create_dir_all(directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!(
                "---\nname: {}\ndescription: {description}\n---\n",
                "sample-skill"
            ),
        )
        .unwrap();
    }
    let entries = discover(&config, &paths).unwrap();
    let creator = entries
        .iter()
        .find(|entry| entry.metadata.name == "sample-skill")
        .unwrap();
    assert_eq!(creator.source, SkillSource::Persona);
    assert_eq!(creator.metadata.description, "persona");
}

#[test]
fn create_and_publish_draft_never_overwrites() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let draft = create_draft(
        &config,
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Global,
    )
    .unwrap();
    let published = publish_draft(&paths, &draft.id).unwrap();
    assert!(Path::new(&published.path).join("SKILL.md").is_file());
    assert!(create_draft(
        &config,
        &paths,
        "sample-skill",
        "Duplicate",
        SkillScope::Global,
    )
    .is_err());
}

#[test]
fn deletes_global_and_current_persona_skills() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    for scope in [SkillScope::Global, SkillScope::Persona] {
        let draft = create_draft(
            &config,
            &paths,
            "sample-skill",
            "Use for sample tasks",
            scope,
        )
        .unwrap();
        publish_draft(&paths, &draft.id).unwrap();
    }

    let global = delete_skill(&config, &paths, "sample-skill", SkillScope::Global).unwrap();
    assert_eq!(global.scope, "global");
    assert!(!paths.skills_dir.join("sample-skill").exists());
    assert!(config
        .active_persona_skills_dir(&paths)
        .join("sample-skill")
        .is_dir());

    let persona = delete_skill(&config, &paths, "sample-skill", SkillScope::Persona).unwrap();
    assert_eq!(persona.scope, "persona");
    assert!(!config
        .active_persona_skills_dir(&paths)
        .join("sample-skill")
        .exists());
    assert!(delete_skill(&config, &paths, "sample-skill", SkillScope::Global).is_err());
}

#[test]
fn update_draft_detects_concurrent_edits() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let created = create_draft(
        &config,
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Global,
    )
    .unwrap();
    publish_draft(&paths, &created.id).unwrap();
    let update = update_draft(&config, &paths, "sample-skill", SkillScope::Global).unwrap();
    fs::write(
        paths.skills_dir.join("sample-skill/SKILL.md"),
        "---\nname: sample-skill\ndescription: Changed elsewhere\n---\n",
    )
    .unwrap();
    assert!(publish_draft(&paths, &update.id).is_err());
}

#[test]
fn two_update_drafts_from_the_same_revision_cannot_both_publish() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let created = create_draft(
        &config,
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Global,
    )
    .unwrap();
    publish_draft(&paths, &created.id).unwrap();
    let first = update_draft(&config, &paths, "sample-skill", SkillScope::Global).unwrap();
    let second = update_draft(&config, &paths, "sample-skill", SkillScope::Global).unwrap();
    fs::write(
        &first.skill_file,
        "---\nname: sample-skill\ndescription: First update\n---\n",
    )
    .unwrap();
    fs::write(
        &second.skill_file,
        "---\nname: sample-skill\ndescription: Second update\n---\n",
    )
    .unwrap();

    publish_draft(&paths, &first.id).unwrap();
    assert!(publish_draft(&paths, &second.id).is_err());
    assert!(
        fs::read_to_string(paths.skills_dir.join("sample-skill/SKILL.md"))
            .unwrap()
            .contains("First update")
    );
}

#[test]
fn live_edit_detected_after_exchange_is_atomically_restored() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("sample-skill");
    let staged = temp.path().join(".stage");
    for (directory, description) in [(&target, "Original"), (&staged, "Updated")] {
        fs::create_dir(directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: sample-skill\ndescription: {description}\n---\n"),
        )
        .unwrap();
    }
    let expected = skill_revision(&target).unwrap();
    fs::write(
        target.join("SKILL.md"),
        "---\nname: sample-skill\ndescription: Manual edit\n---\n",
    )
    .unwrap();

    let mut guard = StagedDirectory::new(staged.clone());
    assert!(install_updated_skill(&staged, &target, &expected, &mut guard).is_err());
    assert!(fs::read_to_string(target.join("SKILL.md"))
        .unwrap()
        .contains("Manual edit"));
    assert!(fs::read_to_string(staged.join("SKILL.md"))
        .unwrap()
        .contains("Updated"));
}

#[test]
fn tampered_persona_scope_cannot_escape_the_skill_root() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let draft = create_draft(
        &config,
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Persona,
    )
    .unwrap();
    let manifest_path = paths
        .skill_drafts_dir()
        .join(&draft.id)
        .join(DRAFT_MANIFEST);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["persona_scope"] = serde_json::Value::String("../../outside".to_string());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    assert!(publish_draft(&paths, &draft.id).is_err());
    assert!(!paths.data_dir.join("outside/sample-skill").exists());
}

#[cfg(unix)]
#[test]
fn publish_rejects_a_symlinked_draft_package() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let draft = create_draft(
        &config,
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Global,
    )
    .unwrap();
    let draft_root = paths.skill_drafts_dir().join(&draft.id);
    let package = draft_root.join(DRAFT_PACKAGE_DIR);
    let outside = temp.path().join("outside-package");
    fs::create_dir_all(outside.join("sample-skill")).unwrap();
    fs::write(
        outside.join("sample-skill/SKILL.md"),
        "---\nname: sample-skill\ndescription: Outside\n---\n",
    )
    .unwrap();
    fs::remove_dir_all(&package).unwrap();
    symlink(&outside, &package).unwrap();

    assert!(publish_draft(&paths, &draft.id).is_err());
    assert!(!paths.skills_dir.join("sample-skill").exists());
}

#[test]
fn expired_draft_cannot_be_published_directly() {
    fn set_modified_recursive(path: &Path, modified: SystemTime) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                set_modified_recursive(&entry.unwrap().path(), modified);
            }
        }
        File::open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let draft = create_draft(
        &config,
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Global,
    )
    .unwrap();
    let draft_root = paths.skill_drafts_dir().join(&draft.id);
    let expired = SystemTime::now() - DRAFT_RETENTION - Duration::from_secs(60);
    set_modified_recursive(&draft_root, expired);

    assert!(publish_draft(&paths, &draft.id).is_err());
    assert!(!draft_root.exists());
    assert!(!paths.skills_dir.join("sample-skill").exists());
}

#[test]
fn malformed_over_limit_draft_is_removed_during_pruning() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let draft = create_draft(
        &AppConfig::default(),
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Global,
    )
    .unwrap();
    let mut directory = PathBuf::from(&draft.skill_dir);
    for index in 0..=(MAX_SKILL_PACKAGE_DEPTH + 5) {
        directory.push(format!("level-{index}"));
    }
    fs::create_dir_all(directory).unwrap();

    assert_eq!(prune_expired_drafts(&paths).unwrap(), 1);
    assert!(!paths.skill_drafts_dir().join(&draft.id).exists());
}

#[test]
fn future_draft_timestamps_are_not_treated_as_expired() {
    fn set_modified_recursive(path: &Path, modified: SystemTime) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                set_modified_recursive(&entry.unwrap().path(), modified);
            }
        }
        File::open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let draft = create_draft(
        &AppConfig::default(),
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Global,
    )
    .unwrap();
    let draft_root = paths.skill_drafts_dir().join(&draft.id);
    set_modified_recursive(
        &draft_root,
        SystemTime::now() + Duration::from_secs(24 * 60 * 60),
    );

    assert_eq!(prune_expired_drafts(&paths).unwrap(), 0);
    assert!(draft_root.is_dir());
}

#[cfg(unix)]
#[test]
fn revision_tracks_empty_directories_and_executable_bits() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sample-skill");
    fs::create_dir_all(&root).unwrap();
    let skill_file = root.join("SKILL.md");
    fs::write(
        &skill_file,
        "---\nname: sample-skill\ndescription: Sample\n---\n",
    )
    .unwrap();
    let initial = skill_revision(&root).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    let with_directory = skill_revision(&root).unwrap();
    assert_ne!(initial, with_directory);
    let mut permissions = fs::metadata(&skill_file).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&skill_file, permissions).unwrap();
    assert_ne!(with_directory, skill_revision(&root).unwrap());
}

#[test]
fn publish_rejects_excessive_directory_depth() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let draft = create_draft(
        &config,
        &paths,
        "sample-skill",
        "Use for sample tasks",
        SkillScope::Global,
    )
    .unwrap();
    let mut directory = PathBuf::from(&draft.skill_dir);
    for index in 0..=MAX_SKILL_PACKAGE_DEPTH {
        directory.push(format!("level-{index}"));
    }
    fs::create_dir_all(directory).unwrap();

    assert!(publish_draft(&paths, &draft.id).is_err());
    assert!(!paths.skills_dir.join("sample-skill").exists());
}
