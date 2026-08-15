//! parse — 自 src/skills.rs 拆分。

use super::*;

pub fn parse_skill_metadata(raw: &str, expected_name: Option<&str>) -> Result<SkillMetadata> {
    parse_skill_document(raw, expected_name).map(|(metadata, _)| metadata)
}

pub(crate) fn parse_skill_document(raw: &str, expected_name: Option<&str>) -> Result<(SkillMetadata, String)> {
    let (frontmatter, body) = split_frontmatter(raw)?;
    validate_frontmatter_tokens(&frontmatter)?;
    let documents =
        YamlLoader::load_from_str(&frontmatter).context("parsing skill YAML frontmatter")?;
    if documents.len() != 1 {
        bail!("skill frontmatter must contain exactly one YAML document");
    }
    let mapping = documents[0]
        .as_hash()
        .context("skill frontmatter root must be a mapping")?;
    let name = required_yaml_string(mapping, "name")?;
    validate_skill_name(&name)?;
    if let Some(expected) = expected_name {
        if name != expected {
            bail!("skill name '{name}' does not match directory '{expected}'");
        }
    }
    let description = required_yaml_string(mapping, "description")?;
    validate_description(&description)?;
    let license = optional_yaml_string(mapping, "license")?;
    let compatibility = optional_yaml_string(mapping, "compatibility")?;
    if compatibility
        .as_ref()
        .is_some_and(|value| !(1..=500).contains(&value.chars().count()))
    {
        bail!("skill compatibility must be 1-500 characters");
    }
    let allowed_tools = optional_yaml_string(mapping, "allowed-tools")?;
    let metadata = yaml_string_map(mapping, "metadata")?;
    Ok((
        SkillMetadata {
            name,
            description,
            license,
            compatibility,
            metadata,
            allowed_tools,
        },
        body,
    ))
}

pub(crate) fn validate_frontmatter_tokens(frontmatter: &str) -> Result<()> {
    for (index, Token(_, token)) in Scanner::new(frontmatter.chars()).enumerate() {
        if index >= MAX_YAML_TOKENS {
            bail!("skill frontmatter exceeds the YAML token limit");
        }
        if matches!(token, TokenType::Alias(_) | TokenType::Anchor(_)) {
            bail!("skill frontmatter may not use YAML anchors or aliases");
        }
    }
    Ok(())
}

pub(crate) fn split_frontmatter(raw: &str) -> Result<(String, String)> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines();
    if lines.next() != Some("---") {
        bail!("SKILL.md must begin with YAML frontmatter");
    }
    let mut frontmatter = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line);
    }
    if !closed {
        bail!("SKILL.md frontmatter is missing its closing ---");
    }
    Ok((frontmatter.join("\n"), lines.collect::<Vec<_>>().join("\n")))
}

pub(crate) fn required_yaml_string(mapping: &yaml_rust2::yaml::Hash, key: &str) -> Result<String> {
    optional_yaml_string(mapping, key)?.ok_or_else(|| anyhow::anyhow!("skill {key} is required"))
}

pub(crate) fn optional_yaml_string(mapping: &yaml_rust2::yaml::Hash, key: &str) -> Result<Option<String>> {
    let Some(value) = mapping.get(&Yaml::String(key.to_string())) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("skill {key} must be a string"))?
        .trim()
        .to_string();
    if value.is_empty() {
        bail!("skill {key} must not be empty");
    }
    Ok(Some(value))
}

pub(crate) fn yaml_string_map(
    mapping: &yaml_rust2::yaml::Hash,
    key: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = mapping.get(&Yaml::String(key.to_string())) else {
        return Ok(BTreeMap::new());
    };
    let values = value
        .as_hash()
        .ok_or_else(|| anyhow::anyhow!("skill {key} must be a string-to-string mapping"))?;
    let mut result = BTreeMap::new();
    for (name, value) in values {
        let name = name
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("skill {key} keys must be strings"))?;
        let value = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("skill {key} values must be strings"))?;
        result.insert(name.to_string(), value.to_string());
    }
    Ok(result)
}

pub(crate) fn validate_skill_name(name: &str) -> Result<()> {
    let len = name.chars().count();
    if !(1..=64).contains(&len) || name.starts_with('-') || name.ends_with('-') {
        bail!("skill name must be 1-64 lowercase ASCII letters, digits, or single hyphens");
    }
    let mut previous_hyphen = false;
    for character in name.chars() {
        let valid =
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-';
        if !valid || character == '-' && previous_hyphen {
            bail!("skill name must be 1-64 lowercase ASCII letters, digits, or single hyphens");
        }
        previous_hyphen = character == '-';
    }
    Ok(())
}

pub(crate) fn validate_description(description: &str) -> Result<()> {
    if !(1..=1024).contains(&description.trim().chars().count()) {
        bail!("skill description must be 1-1024 characters");
    }
    Ok(())
}

pub(crate) fn read_skill_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("SKILL.md must be a regular file: {}", path.display());
    }
    if metadata.len() > MAX_SKILL_FILE_BYTES {
        bail!("SKILL.md exceeds the {MAX_SKILL_FILE_BYTES} byte limit");
    }
    fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

