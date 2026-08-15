//! tests — 自 src/tools/knowledge_base.rs 外移。
#![cfg(test)]

pub(crate) use super::*;

use crate::paths::GQYPaths;

fn test_paths(root: &Path) -> GQYPaths {
    GQYPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("fish/conf.d/gqy.fish"),
        bash_hook_file: root.join("config/shell/bash-hook.sh"),
        zsh_hook_file: root.join("config/shell/zsh-hook.zsh"),
        scripts_dir: root.join("config/scripts"),
        system_scripts_dir: PathBuf::new(),
    }
}

#[test]
fn edit_lines_replaces_inclusive_range() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let kb = KnowledgeBase::new(config, paths).unwrap();
    let source = temp.path().join("note.md");
    std::fs::write(&source, "one\ntwo\nthree\n").unwrap();
    kb.import_file(&source, "notes/note.md").unwrap();

    let result = kb.edit_lines("notes/note.md", 2, 2, "TWO\nTWO-B").unwrap();

    assert_eq!(result.old_line_count, 3);
    assert_eq!(result.new_line_count, 4);
    assert!(!result.semantic_refreshed);
    let edited = std::fs::read_to_string(kb.existing_file_path("notes/note.md").unwrap()).unwrap();
    assert_eq!(edited, "one\nTWO\nTWO-B\nthree\n");
    let chunks: i64 = kb
        .semantic_conn()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM semantic_chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(chunks, 0);
}

#[test]
fn edit_lines_empty_replacement_deletes_range() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let kb = KnowledgeBase::new(config, paths).unwrap();
    let source = temp.path().join("note.md");
    std::fs::write(&source, "one\ntwo\nthree").unwrap();
    kb.import_file(&source, "note.md").unwrap();

    let result = kb.edit_lines("note.md", 2, 3, "").unwrap();

    assert_eq!(result.old_line_count, 3);
    assert_eq!(result.new_line_count, 1);
    let edited = std::fs::read_to_string(kb.existing_file_path("note.md").unwrap()).unwrap();
    assert_eq!(edited, "one");
}

#[test]
fn edit_lines_rejects_out_of_range() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let kb = KnowledgeBase::new(config, paths).unwrap();
    let source = temp.path().join("note.md");
    std::fs::write(&source, "one\n").unwrap();
    kb.import_file(&source, "note.md").unwrap();

    let error = kb.edit_lines("note.md", 2, 2, "two").unwrap_err();

    assert!(error.to_string().contains("out of range"));
}
