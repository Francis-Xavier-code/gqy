//! tests — 自 src/paths.rs 外移。
#![cfg(test)]

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_layouts(root: &Path) -> (LegacyLayout, Layout) {
        (
            LegacyLayout {
                config_dir: root.join("legacy/config"),
                data_dir: root.join("legacy/data"),
                cache_dir: root.join("legacy/cache"),
                state_dir: root.join("legacy/state"),
                documents_dir: root.join("Documents/Miyu"),
                pictures_dirs: vec![root.join("Pictures/miyu"), root.join("Pictures/Miyu")],
            },
            Layout {
                root_dir: root.join(".gqy"),
                config_dir: root.join(".gqy/config"),
                data_dir: root.join(".gqy/data"),
                cache_dir: root.join(".gqy/cache"),
                state_dir: root.join(".gqy/state"),
            },
        )
    }

    #[test]
    fn resource_layout_migration_moves_owned_content_and_commits_marker() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        fs::create_dir_all(layout.config_dir.join("skills/demo")).unwrap();
        fs::create_dir_all(layout.config_dir.join("scripts")).unwrap();
        fs::create_dir_all(layout.config_dir.join("prompts")).unwrap();
        fs::create_dir_all(layout.config_dir.join("identities")).unwrap();
        fs::create_dir_all(layout.config_dir.join("persona-avatars")).unwrap();
        fs::write(
            layout.config_dir.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: Demo\n---\n",
        )
        .unwrap();
        fs::write(layout.config_dir.join("scripts/tool.sh"), "#!/bin/sh\n").unwrap();
        fs::write(layout.config_dir.join("prompts/persona.md"), "persona").unwrap();
        fs::write(layout.config_dir.join("identities/user.md"), "user").unwrap();
        fs::write(
            layout.config_dir.join("persona-avatars/avatar.png"),
            "image",
        )
        .unwrap();
        fs::write(layout.config_dir.join("system-prompt.md"), "system").unwrap();
        fs::write(layout.config_dir.join("user-identity.md"), "legacy user").unwrap();

        migrate_resource_layout(&layout).unwrap();

        assert!(layout.data_dir.join("skills/demo/SKILL.md").is_file());
        assert!(layout.data_dir.join("scripts/tool.sh").is_file());
        assert!(layout.data_dir.join("prompts/persona.md").is_file());
        assert!(layout.data_dir.join("prompts/system-prompt.md").is_file());
        assert!(layout.data_dir.join("identities/user.md").is_file());
        assert!(layout
            .data_dir
            .join("identities/user-identity.md")
            .is_file());
        assert!(layout.data_dir.join("persona-avatars/avatar.png").is_file());
        assert!(layout.resource_marker().is_file());
        assert!(!layout.resource_journal().exists());
    }

    #[test]
    fn resource_layout_conflict_has_no_migration_writes() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        fs::create_dir_all(layout.config_dir.join("skills/demo")).unwrap();
        fs::create_dir_all(layout.data_dir.join("skills/existing")).unwrap();
        fs::write(layout.config_dir.join("skills/demo/SKILL.md"), "source").unwrap();
        fs::write(
            layout.data_dir.join("skills/existing/SKILL.md"),
            "destination",
        )
        .unwrap();

        let error = migrate_resource_layout(&layout).unwrap_err();

        assert!(error.to_string().contains("destination already exists"));
        assert!(layout.config_dir.join("skills/demo/SKILL.md").is_file());
        assert!(layout.data_dir.join("skills/existing/SKILL.md").is_file());
        assert!(!layout.resource_marker().exists());
        assert!(!layout.resource_journal().exists());
    }

    #[test]
    fn resource_layout_journal_rolls_back_interrupted_moves() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        let source = layout.config_dir.join("skills");
        let destination = layout.data_dir.join("skills");
        fs::create_dir_all(source.join("demo")).unwrap();
        fs::create_dir_all(&layout.data_dir).unwrap();
        fs::write(source.join("demo/SKILL.md"), "skill").unwrap();
        fs::rename(&source, &destination).unwrap();
        let journal = ResourceMigrationJournal {
            entries: vec![ResourceMigrationEntry {
                source: source.clone(),
                destination: destination.clone(),
            }],
            moved: 1,
            pending: None,
        };
        fs::create_dir_all(&layout.root_dir).unwrap();
        write_resource_journal(&layout, &journal).unwrap();

        recover_resource_migration(&layout).unwrap();

        assert!(source.join("demo/SKILL.md").is_file());
        assert!(!destination.exists());
        assert!(!layout.resource_journal().exists());
    }

    #[test]
    fn resource_layout_journal_recovers_pending_and_already_rolled_back_entries() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        let source = layout.config_dir.join("skills");
        let destination = layout.data_dir.join("skills");
        fs::create_dir_all(source.join("demo")).unwrap();
        fs::create_dir_all(&layout.data_dir).unwrap();
        fs::write(source.join("demo/SKILL.md"), "skill").unwrap();
        fs::rename(&source, &destination).unwrap();
        let mut journal = ResourceMigrationJournal {
            entries: vec![ResourceMigrationEntry {
                source: source.clone(),
                destination: destination.clone(),
            }],
            moved: 0,
            pending: Some(0),
        };
        fs::create_dir_all(&layout.root_dir).unwrap();
        write_resource_journal(&layout, &journal).unwrap();
        recover_resource_migration(&layout).unwrap();
        assert!(source.join("demo/SKILL.md").is_file());

        journal.moved = 1;
        journal.pending = None;
        write_resource_journal(&layout, &journal).unwrap();
        recover_resource_migration(&layout).unwrap();
        assert!(source.join("demo/SKILL.md").is_file());
        assert!(!destination.exists());
        assert!(!layout.resource_journal().exists());
    }

    #[test]
    fn resource_layout_preflights_overlapping_legacy_files() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        fs::create_dir_all(layout.config_dir.join("prompts")).unwrap();
        fs::write(layout.config_dir.join("prompts/system-prompt.md"), "nested").unwrap();
        fs::write(layout.config_dir.join("system-prompt.md"), "top-level").unwrap();

        let error = migrate_resource_layout(&layout).unwrap_err();

        assert!(error.to_string().contains("overlapping sources"));
        assert!(layout.config_dir.join("prompts/system-prompt.md").is_file());
        assert!(layout.config_dir.join("system-prompt.md").is_file());
        assert!(!layout.resource_journal().exists());
    }

    #[cfg(unix)]
    #[test]
    fn resource_layout_rejects_symbolic_link_destination_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        let outside = temp.path().join("outside");
        fs::create_dir_all(&layout.config_dir).unwrap();
        fs::create_dir_all(&layout.data_dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(layout.config_dir.join("system-prompt.md"), "system").unwrap();
        symlink(&outside, layout.data_dir.join("prompts")).unwrap();

        let error = migrate_resource_layout(&layout).unwrap_err();

        assert!(error
            .to_string()
            .contains("symbolic-link destination ancestor"));
        assert!(layout.config_dir.join("system-prompt.md").is_file());
        assert!(!outside.join("system-prompt.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn resource_layout_rejects_absolute_symlinks_into_moved_trees() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        let skill_file = layout.config_dir.join("skills/demo/SKILL.md");
        fs::create_dir_all(skill_file.parent().unwrap()).unwrap();
        fs::create_dir_all(layout.config_dir.join("prompts")).unwrap();
        fs::write(&skill_file, "skill").unwrap();
        symlink(
            &skill_file,
            layout.config_dir.join("prompts/linked-skill.md"),
        )
        .unwrap();

        let error = migrate_resource_layout(&layout).unwrap_err();

        assert!(error.to_string().contains("absolute target moves"));
        assert!(skill_file.is_file());
        assert!(!layout.resource_marker().exists());
    }

    #[test]
    fn migration_moves_and_merges_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("legacy");
        let destination = temp.path().join("next");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("nested/value.txt"), "old").unwrap();
        fs::write(destination.join("kept.txt"), "new").unwrap();

        migrate_entry(&source, &destination).unwrap();

        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(destination.join("nested/value.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(destination.join("kept.txt")).unwrap(),
            "new"
        );
    }

    #[test]
    fn migration_preflights_conflicts_then_removes_identical_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("legacy");
        let destination = temp.path().join("next");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("same.txt"), "same").unwrap();
        fs::write(destination.join("same.txt"), "same").unwrap();
        fs::write(source.join("conflict.txt"), "legacy").unwrap();
        fs::write(destination.join("conflict.txt"), "next").unwrap();

        let error = migrate_entry(&source, &destination).unwrap_err();

        assert!(source.join("same.txt").exists());
        assert!(source.join("conflict.txt").exists());
        assert_eq!(
            fs::read_to_string(destination.join("conflict.txt")).unwrap(),
            "next"
        );
        assert!(error.to_string().contains("conflicting entries"));

        fs::remove_file(source.join("conflict.txt")).unwrap();
        migrate_entry(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(destination.join("same.txt")).unwrap(),
            "same"
        );
    }

    #[test]
    fn unified_layout_merges_xdg_documents_and_both_picture_directories() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        for directory in [
            &legacy.config_dir,
            &legacy.data_dir,
            &legacy.cache_dir,
            &legacy.state_dir,
            &legacy.documents_dir,
            &legacy.pictures_dirs[0],
            &legacy.pictures_dirs[1],
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::create_dir_all(legacy.data_dir.join("documents")).unwrap();
        fs::create_dir_all(legacy.data_dir.join("pictures")).unwrap();
        fs::create_dir_all(legacy.config_dir.join("shell")).unwrap();
        fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
        fs::write(legacy.config_dir.join("shell/bash-hook.sh"), "bash hook").unwrap();
        fs::write(legacy.config_dir.join("shell/zsh-hook.zsh"), "zsh hook").unwrap();
        fs::write(
            temp.path().join(".bashrc"),
            "before\n# >>> gqy bash hook >>>\nsource '/legacy/bash-hook.sh'\n# <<< gqy bash hook <<<\nafter\n",
        )
        .unwrap();
        fs::write(
            temp.path().join(".zshrc"),
            "before\n# >>> gqy zsh hook >>>\nsource '/legacy/zsh-hook.zsh'\n# <<< gqy zsh hook <<<\nafter\n",
        )
        .unwrap();
        fs::write(legacy.data_dir.join("data.bin"), "data").unwrap();
        fs::write(legacy.cache_dir.join("cache.bin"), "cache").unwrap();
        fs::write(legacy.state_dir.join("state.db"), "state").unwrap();
        fs::write(legacy.documents_dir.join("report.md"), "report").unwrap();
        fs::write(
            legacy.data_dir.join("documents/shared.txt"),
            "same document",
        )
        .unwrap();
        fs::write(legacy.documents_dir.join("shared.txt"), "same document").unwrap();
        fs::write(legacy.data_dir.join("pictures/shared.png"), "same picture").unwrap();
        fs::write(legacy.pictures_dirs[0].join("shared.png"), "same picture").unwrap();
        fs::write(legacy.pictures_dirs[0].join("lower.png"), "lower").unwrap();
        fs::write(legacy.pictures_dirs[1].join("shared.png"), "same picture").unwrap();
        fs::write(legacy.pictures_dirs[1].join("upper.png"), "upper").unwrap();

        migrate_legacy_layout(&legacy, &next).unwrap();

        for source in [
            &legacy.config_dir,
            &legacy.data_dir,
            &legacy.cache_dir,
            &legacy.state_dir,
            &legacy.documents_dir,
            &legacy.pictures_dirs[0],
            &legacy.pictures_dirs[1],
        ] {
            assert!(
                !source.exists(),
                "legacy source remains: {}",
                source.display()
            );
        }
        assert_eq!(
            fs::read_to_string(next.config_dir.join("config.jsonc")).unwrap(),
            "config"
        );
        assert_eq!(
            fs::read_to_string(next.config_dir.join("shell/bash-hook.sh")).unwrap(),
            "bash hook"
        );
        assert_eq!(
            fs::read_to_string(next.config_dir.join("shell/zsh-hook.zsh")).unwrap(),
            "zsh hook"
        );
        let bashrc = fs::read_to_string(temp.path().join(".bashrc")).unwrap();
        assert!(bashrc.contains(
            next.config_dir
                .join("shell/bash-hook.sh")
                .to_string_lossy()
                .as_ref()
        ));
        assert!(!bashrc.contains("/legacy/bash-hook.sh"));
        let zshrc = fs::read_to_string(temp.path().join(".zshrc")).unwrap();
        assert!(zshrc.contains(
            next.config_dir
                .join("shell/zsh-hook.zsh")
                .to_string_lossy()
                .as_ref()
        ));
        assert!(!zshrc.contains("/legacy/zsh-hook.zsh"));
        assert_eq!(
            fs::read_to_string(next.data_dir.join("data.bin")).unwrap(),
            "data"
        );
        assert_eq!(
            fs::read_to_string(next.cache_dir.join("cache.bin")).unwrap(),
            "cache"
        );
        assert_eq!(
            fs::read_to_string(next.state_dir.join("state.db")).unwrap(),
            "state"
        );
        assert_eq!(
            fs::read_to_string(next.data_dir.join("documents/report.md")).unwrap(),
            "report"
        );
        assert_eq!(
            fs::read_to_string(next.data_dir.join("documents/shared.txt")).unwrap(),
            "same document"
        );
        assert_eq!(
            fs::read_to_string(next.data_dir.join("pictures/shared.png")).unwrap(),
            "same picture"
        );
        assert_eq!(
            fs::read_to_string(next.data_dir.join("pictures/lower.png")).unwrap(),
            "lower"
        );
        assert_eq!(
            fs::read_to_string(next.data_dir.join("pictures/upper.png")).unwrap(),
            "upper"
        );
        assert!(next.marker().exists());
    }

    #[test]
    fn unified_layout_discards_cache_with_relative_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        fs::create_dir_all(&legacy.config_dir).unwrap();
        fs::create_dir_all(legacy.cache_dir.join("blobs")).unwrap();
        fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
        fs::write(legacy.cache_dir.join("blobs/blob"), "blob").unwrap();
        symlink("blobs/blob", legacy.cache_dir.join("snapshot")).unwrap();

        migrate_legacy_layout(&legacy, &next).unwrap();

        assert!(!legacy.cache_dir.exists());
        assert!(!next.cache_dir.join("snapshot").exists());
        assert_eq!(
            fs::read_to_string(next.config_dir.join("config.jsonc")).unwrap(),
            "config"
        );
        assert!(next.marker().exists());
    }

    #[test]
    fn unified_layout_discards_symlinked_cache_without_touching_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        fs::create_dir_all(&legacy.config_dir).unwrap();
        fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
        let target = temp.path().join("real-cache");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("cache.bin"), "keep me").unwrap();
        symlink("../real-cache", &legacy.cache_dir).unwrap();

        migrate_legacy_layout(&legacy, &next).unwrap();

        assert!(fs::symlink_metadata(&legacy.cache_dir).is_err());
        assert_eq!(
            fs::read_to_string(target.join("cache.bin")).unwrap(),
            "keep me"
        );
        assert!(next.marker().exists());
    }

    #[test]
    fn unified_layout_discards_cache_whose_absolute_symlink_target_moves() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        fs::create_dir_all(&legacy.config_dir).unwrap();
        fs::create_dir_all(&legacy.data_dir).unwrap();
        fs::create_dir_all(&legacy.cache_dir).unwrap();
        fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
        fs::write(legacy.data_dir.join("data.bin"), "data").unwrap();
        symlink(
            legacy.data_dir.join("data.bin"),
            legacy.cache_dir.join("link"),
        )
        .unwrap();

        migrate_legacy_layout(&legacy, &next).unwrap();

        assert!(!legacy.cache_dir.exists());
        assert_eq!(
            fs::read_to_string(next.data_dir.join("data.bin")).unwrap(),
            "data"
        );
        assert!(next.marker().exists());
    }

    #[test]
    fn unified_layout_still_refuses_relative_symlink_outside_cache() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        fs::create_dir_all(&legacy.config_dir).unwrap();
        fs::create_dir_all(&legacy.cache_dir).unwrap();
        fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
        fs::write(legacy.cache_dir.join("cache.bin"), "cache").unwrap();
        symlink("config.jsonc", legacy.config_dir.join("alias")).unwrap();

        let error = migrate_legacy_layout(&legacy, &next).unwrap_err();

        assert!(error.to_string().contains("relative symbolic link"));
        // A healthy cache must survive an aborted migration untouched.
        assert_eq!(
            fs::read_to_string(legacy.cache_dir.join("cache.bin")).unwrap(),
            "cache"
        );
        assert!(!next.root_dir.exists());
    }

    #[test]
    fn unified_layout_cross_source_conflict_has_zero_migration_writes() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        fs::create_dir_all(&legacy.config_dir).unwrap();
        fs::create_dir_all(&legacy.pictures_dirs[0]).unwrap();
        fs::create_dir_all(&legacy.pictures_dirs[1]).unwrap();
        fs::write(legacy.config_dir.join("config.jsonc"), "must stay").unwrap();
        fs::write(legacy.pictures_dirs[0].join("conflict.png"), "lower").unwrap();
        fs::write(legacy.pictures_dirs[1].join("conflict.png"), "upper").unwrap();

        let error = migrate_legacy_layout(&legacy, &next).unwrap_err();

        assert!(error.to_string().contains("conflicting legacy entries"));
        assert_eq!(
            fs::read_to_string(legacy.config_dir.join("config.jsonc")).unwrap(),
            "must stay"
        );
        assert_eq!(
            fs::read_to_string(legacy.pictures_dirs[0].join("conflict.png")).unwrap(),
            "lower"
        );
        assert_eq!(
            fs::read_to_string(legacy.pictures_dirs[1].join("conflict.png")).unwrap(),
            "upper"
        );
        assert!(!next.root_dir.exists());
    }

    #[test]
    fn unified_layout_late_destination_conflict_does_not_move_earlier_sources() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        fs::create_dir_all(&legacy.config_dir).unwrap();
        fs::create_dir_all(&legacy.documents_dir).unwrap();
        fs::create_dir_all(next.data_dir.join("documents")).unwrap();
        fs::write(legacy.config_dir.join("config.jsonc"), "must stay").unwrap();
        fs::write(legacy.documents_dir.join("conflict.md"), "legacy").unwrap();
        fs::write(next.data_dir.join("documents/conflict.md"), "current").unwrap();

        let error = migrate_legacy_layout(&legacy, &next).unwrap_err();

        assert!(error.to_string().contains("conflicting entries"));
        assert!(legacy.config_dir.join("config.jsonc").exists());
        assert!(!next.config_dir.exists());
        assert_eq!(
            fs::read_to_string(legacy.documents_dir.join("conflict.md")).unwrap(),
            "legacy"
        );
        assert_eq!(
            fs::read_to_string(next.data_dir.join("documents/conflict.md")).unwrap(),
            "current"
        );
        assert!(!next.marker().exists());
    }

    #[test]
    fn interrupted_shell_hook_refresh_is_retried_before_marking_layout_complete() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        fs::create_dir_all(legacy.config_dir.join("shell")).unwrap();
        fs::write(legacy.config_dir.join("shell/bash-hook.sh"), "bash hook").unwrap();
        fs::write(
            temp.path().join(".bashrc"),
            "# >>> gqy bash hook >>>\nsource '/legacy/bash-hook.sh'\n",
        )
        .unwrap();

        let error = migrate_legacy_layout(&legacy, &next).unwrap_err();
        assert!(error.to_string().contains("refreshing shell hook paths"));
        assert!(!next.marker().exists());
        assert!(next.config_dir.join("shell/bash-hook.sh").exists());

        fs::write(
            temp.path().join(".bashrc"),
            "# >>> gqy bash hook >>>\nsource '/legacy/bash-hook.sh'\n# <<< gqy bash hook <<<\n",
        )
        .unwrap();
        migrate_legacy_layout(&legacy, &next).unwrap();

        let bashrc = fs::read_to_string(temp.path().join(".bashrc")).unwrap();
        assert!(bashrc.contains(
            next.config_dir
                .join("shell/bash-hook.sh")
                .to_string_lossy()
                .as_ref()
        ));
        assert!(!bashrc.contains("/legacy/bash-hook.sh"));
        assert!(next.marker().exists());
    }

    #[test]
    fn migration_rejects_relative_symbolic_links() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("legacy");
        let destination = temp.path().join("next");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(temp.path().join("outside")).unwrap();
        fs::write(temp.path().join("outside/value"), "value").unwrap();
        symlink("../outside/value", source.join("link")).unwrap();

        let error = migrate_entry(&source, &destination).unwrap_err();

        assert!(error.to_string().contains("relative symbolic link"));
        assert!(source.join("link").exists());
        assert!(!destination.exists());
    }

    #[test]
    fn migration_rejects_symlinked_destination_root_and_marker() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, next) = test_layouts(temp.path());
        fs::create_dir_all(&legacy.config_dir).unwrap();
        fs::write(legacy.config_dir.join("config.jsonc"), "config").unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();

        symlink(&outside, &next.root_dir).unwrap();
        let root_error = migrate_legacy_layout(&legacy, &next).unwrap_err();
        assert!(root_error.to_string().contains("symbolic-link directory"));
        fs::remove_file(&next.root_dir).unwrap();

        fs::create_dir_all(&next.root_dir).unwrap();
        let marker_target = temp.path().join("marker-target");
        fs::write(&marker_target, "1\n").unwrap();
        symlink(&marker_target, next.marker()).unwrap();
        let marker_error = migrate_legacy_layout(&legacy, &next).unwrap_err();
        assert!(marker_error.to_string().contains("layout marker"));
    }

    #[test]
    fn legacy_data_and_state_alias_is_migrated_once() {
        let temp = tempfile::tempdir().unwrap();
        let (mut legacy, next) = test_layouts(temp.path());
        legacy.state_dir = legacy.data_dir.clone();
        fs::create_dir_all(&legacy.data_dir).unwrap();
        fs::write(legacy.data_dir.join("shared.db"), "state and data").unwrap();

        migrate_legacy_layout(&legacy, &next).unwrap();

        assert_eq!(
            fs::read_to_string(next.data_dir.join("shared.db")).unwrap(),
            "state and data"
        );
        assert!(!legacy.data_dir.exists());
    }

    #[test]
    fn default_runtime_directory_keeps_the_legacy_name() {
        let runtime_root = Path::new("/run/user/1000");
        assert_eq!(
            runtime_dir_for(runtime_root, None),
            runtime_root.join("gqy")
        );
    }

    #[test]
    fn explicit_homes_get_stable_isolated_runtime_directories() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_root = temp.path().join("runtime");
        let first = temp.path().join("homes/first");
        let second = temp.path().join("homes/second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        let first_runtime = runtime_dir_for(&runtime_root, Some(&first));
        assert_eq!(first_runtime, runtime_dir_for(&runtime_root, Some(&first)));
        assert_ne!(first_runtime, runtime_dir_for(&runtime_root, Some(&second)));
        assert!(first_runtime
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("gqy-"));
    }

    #[test]
    fn resource_path_remapping_includes_the_legacy_xdg_config_root() {
        let base = BaseDirs::new().unwrap();
        let root = base.home_dir().join(".gqy");
        let paths = GQYPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("data/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("data/pictures"),
            fish_hook_file: base.config_dir().join("fish/conf.d/gqy.fish"),
            bash_hook_file: root.join("config/shell/bash-hook.sh"),
            zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
            scripts_dir: root.join("data/scripts"),
            system_scripts_dir: PathBuf::from("/usr/share/gqy/scripts"),
        };
        assert_eq!(
            paths.migrated_resource_path(&base.config_dir().join("gqy/prompts/team")),
            Some(root.join("data/prompts/team"))
        );
        assert_eq!(
            paths.migrated_resource_path(Path::new("prompts/../scripts/images")),
            Some(root.join("data/scripts/images"))
        );
    }

    #[test]
    fn runtime_hash_uses_a_normalized_home_path() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(home.join("child")).unwrap();
        let equivalent = home.join("child/..");

        assert_eq!(
            runtime_dir_for(temp.path(), Some(&home)),
            runtime_dir_for(temp.path(), Some(&equivalent))
        );
    }

    #[test]
    fn legacy_layout_stays_put_while_the_core_lock_is_held() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, _) = test_layouts(temp.path());
        let runtime_dir = legacy.state_dir.join("gqy");
        fs::create_dir_all(&runtime_dir).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(runtime_dir.join("core.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );

        assert!(legacy_daemon_is_running_at(&legacy, None));
        unsafe {
            libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
        }
        assert!(!legacy_daemon_is_running_at(&legacy, None));
    }

    #[test]
    fn legacy_layout_stays_put_while_the_starter_lock_is_held() {
        let temp = tempfile::tempdir().unwrap();
        let (legacy, _) = test_layouts(temp.path());
        let runtime_dir = legacy.state_dir.join("gqy");
        fs::create_dir_all(&runtime_dir).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(runtime_dir.join("starter.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );

        assert!(legacy_daemon_is_running_at(&legacy, None));
        unsafe {
            libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
        }
        assert!(!legacy_daemon_is_running_at(&legacy, None));
    }

    #[test]
    fn resource_migration_defers_for_starters_except_inside_the_spawned_daemon() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_dir = temp.path().join("runtime");
        fs::create_dir_all(&runtime_dir).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(runtime_dir.join("starter.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );

        assert!(daemon_is_running_at(&runtime_dir, false));
        assert!(!daemon_is_running_at(&runtime_dir, true));
    }

    #[test]
    fn resource_migration_holds_runtime_exclusion_through_commit() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        fs::create_dir_all(layout.config_dir.join("skills/demo")).unwrap();
        fs::write(layout.config_dir.join("skills/demo/SKILL.md"), "skill").unwrap();
        let runtime_dir = layout.state_dir.join("gqy");
        fs::create_dir_all(&runtime_dir).unwrap();
        let starter = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(runtime_dir.join("starter.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(starter.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );

        assert!(!try_migrate_resource_layout(&layout, false).unwrap());
        assert!(layout.config_dir.join("skills/demo/SKILL.md").is_file());
        assert!(try_migrate_resource_layout(&layout, true).unwrap());
        assert!(layout.data_dir.join("skills/demo/SKILL.md").is_file());
        assert!(layout.resource_marker().is_file());
    }

    #[test]
    fn concurrent_client_waits_for_resource_migration_marker() {
        let temp = tempfile::tempdir().unwrap();
        let (_, layout) = test_layouts(temp.path());
        fs::create_dir_all(&layout.root_dir).unwrap();
        let lease = acquire_migration_lock(&layout.root_dir).unwrap();
        let concurrent_layout = layout.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            sender
                .send(try_migrate_resource_layout(&concurrent_layout, false))
                .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(receiver.try_recv().is_err());

        write_marker(&layout.resource_marker()).unwrap();
        drop(lease);
        assert!(receiver.recv().unwrap().unwrap());
        thread.join().unwrap();
    }
}
