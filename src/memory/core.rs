//! core — 自 src/memory/mod.rs 拆分。

use super::*;

impl CompactJieba {
    pub(crate) fn new() -> Result<Self> {
        let total_bytes: [u8; 8] = JIEBA_INDEX
            .get(..8)
            .context("compact Jieba index is truncated")?
            .try_into()
            .expect("the total-frequency slice has a fixed length");
        let total = u64::from_le_bytes(total_bytes);
        if total == 0 {
            bail!("compact Jieba index has an empty frequency total");
        }
        let max_word_chars = u32::from_le_bytes(
            JIEBA_INDEX
                .get(8..12)
                .context("compact Jieba index has no maximum word length")?
                .try_into()
                .expect("the maximum-word slice has a fixed length"),
        ) as usize;
        if max_word_chars == 0 {
            bail!("compact Jieba index has an invalid maximum word length");
        }
        Ok(Self {
            words: fst::Map::new(&JIEBA_INDEX[12..]).context("opening compact Jieba index")?,
            log_total: (total as f64).ln(),
            max_word_chars,
        })
    }

    pub(crate) fn cut<'a>(&self, text: &'a str) -> Vec<&'a str> {
        let mut words = Vec::new();
        let mut block_start = None;
        for (index, character) in text.char_indices() {
            if jieba_block_character(character) {
                block_start.get_or_insert(index);
                continue;
            }
            if let Some(start) = block_start.take() {
                self.cut_block(&text[start..index], &mut words);
            }
            let end = index + character.len_utf8();
            words.push(&text[index..end]);
        }
        if let Some(start) = block_start {
            self.cut_block(&text[start..], &mut words);
        }
        words
    }

    pub(crate) fn cut_block<'a>(&self, block: &'a str, words: &mut Vec<&'a str>) {
        let mut boundaries = block
            .char_indices()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        boundaries.push(block.len());
        if boundaries.len() <= 1 {
            return;
        }
        let character_count = boundaries.len() - 1;
        let mut route = vec![(0.0_f64, character_count); character_count + 1];
        for start in (0..character_count).rev() {
            let mut best = (-self.log_total + route[start + 1].0, start + 1);
            let candidate_end = start
                .saturating_add(self.max_word_chars)
                .min(character_count);
            for end in start + 1..=candidate_end {
                let candidate = &block[boundaries[start]..boundaries[end]];
                let Some(frequency) = self.words.get(candidate) else {
                    continue;
                };
                let score = (frequency.max(1) as f64).ln() - self.log_total + route[end].0;
                if score > best.0 || (score == best.0 && end > best.1) {
                    best = (score, end);
                }
            }
            route[start] = best;
        }

        let mut start = 0;
        let mut ascii_start = None;
        while start < character_count {
            let end = route[start].1;
            let token = &block[boundaries[start]..boundaries[end]];
            if token.len() == 1 && token.as_bytes()[0].is_ascii_alphanumeric() {
                ascii_start.get_or_insert(boundaries[start]);
            } else {
                if let Some(byte_start) = ascii_start.take() {
                    words.push(&block[byte_start..boundaries[start]]);
                }
                words.push(token);
            }
            start = end;
        }
        if let Some(byte_start) = ascii_start {
            words.push(&block[byte_start..]);
        }
    }
}

pub(crate) fn jieba_block_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '+' | '#' | '&' | '.' | '_' | '%' | '-')
        || matches!(
            character as u32,
            0x3400..=0x4dbf
                | 0x4e00..=0x9fff
                | 0xf900..=0xfaff
                | 0x20000..=0x2fa1f
        )
}

#[derive(Clone)]
pub struct MemoryStore {
    config: MemoryConfig,
    kb_config: KnowledgeBasePluginConfig,
    /// Kept whole because the embedding call needs provider lookup and the
    /// knowledge base's timeout setting.
    app_config: AppConfig,
    writes_enabled: bool,
    access: MemoryAccess,
    writer_principal: Option<String>,
    writer_display_name: String,
    data_db: PathBuf,
    state_db: PathBuf,
    skills_dir: PathBuf,
}

/// Read authorization for one agent run. Storage remains persona-global; this
/// value only controls which rows may enter the model context or memory tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MemoryAccess {
    Privileged,
    Principal(String),
}

impl MemoryAccess {
    pub(crate) fn principal(key: impl Into<String>) -> Self {
        Self::Principal(key.into())
    }

    pub(crate) fn principal_key(&self) -> Option<&str> {
        match self {
            Self::Privileged => None,
            Self::Principal(key) => Some(key),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MemoryOwnership {
    visibility: &'static str,
    owner_principal: String,
    owner_display_name: String,
}

impl MemoryOwnership {
    pub(crate) fn public() -> Self {
        Self {
            visibility: VISIBILITY_PUBLIC,
            owner_principal: String::new(),
            owner_display_name: String::new(),
        }
    }

    pub(crate) fn privileged() -> Self {
        Self {
            visibility: VISIBILITY_PRIVILEGED,
            owner_principal: String::new(),
            owner_display_name: String::new(),
        }
    }

    pub(crate) fn principal(key: impl Into<String>, display_name: impl Into<String>) -> Self {
        let display_name = display_name.into();
        Self {
            visibility: VISIBILITY_PRINCIPAL,
            owner_principal: key.into(),
            owner_display_name: truncate_chars(&compact_line(&display_name), 128),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EvictedTurn {
    pub source_id: String,
    pub timestamp: String,
    pub role: String,
    pub content: String,
    pub visibility: String,
    pub owner_principal: String,
    pub owner_display_name: String,
}

#[derive(Debug, Clone)]
/// 联想召回的自回声排除:`session_id` 会话里、`since`(最老可见轮的
/// 时间戳,Utc RFC3339,与记忆行同源可比)之后产生的记忆不注入。
pub struct AssociationExclusion {
    pub session_id: String,
    pub since: String,
}

pub struct AssociationContext {
    pub facts: Vec<MemoryHit>,
    pub episodes: Vec<MemoryHit>,
    pub(crate) organization_due: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MemoryKind {
    Fact,
    Diary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct MemoryOrigin {
    pub(crate) kind: String,
    pub(crate) platform: String,
    pub(crate) account_id: String,
    pub(crate) conversation_kind: String,
    pub(crate) conversation_id: String,
    pub(crate) sender_id: String,
    pub(crate) sender_display_name: String,
    pub(crate) session_id: String,
    pub(crate) message_id: String,
}

impl MemoryOrigin {
    pub(crate) fn local(session_id: impl Into<String>) -> Self {
        Self {
            kind: "local".to_string(),
            session_id: session_id.into(),
            ..Self::default()
        }
    }

    pub(crate) fn principal_ownership(&self) -> Option<MemoryOwnership> {
        if self.kind != "platform"
            || self.platform.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.sender_id.trim().is_empty()
        {
            return None;
        }
        Some(MemoryOwnership::principal(
            PlatformPrincipal {
                platform: self.platform.clone(),
                account_id: self.account_id.clone(),
                user_id: self.sender_id.clone(),
            }
            .stable_key(),
            self.sender_display_name.trim(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub id: i64,
    pub kind: MemoryKind,
    pub content: String,
    pub score: f32,
    pub timestamp: String,
    pub source: String,
    pub retention: Option<String>,
    visibility: String,
    owner_principal: String,
    owner_display_name: String,
    subjects: String,
    source_episode_ids: Vec<i64>,
    origin_session_id: String,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub(crate) struct MemorySubject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ShortDiaryRecord {
    pub(crate) id: i64,
    pub(crate) created_at: String,
    pub(crate) user_message: String,
    pub(crate) assistant_message: String,
    pub(crate) force_long_term: bool,
    pub(crate) owner_principal: Option<String>,
    pub(crate) origin: MemoryOrigin,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExistingMemoryRecord {
    pub(crate) id: i64,
    pub(crate) kind: String,
    pub(crate) content: String,
    pub(crate) truth_status: String,
    pub(crate) visibility: String,
    pub(crate) owner_principal: String,
    pub(crate) owner_display_name: String,
}

#[derive(Debug, Clone)]
pub(crate) struct OrganizationBatch {
    pub(crate) database_id: String,
    pub(crate) generation: i64,
    pub(crate) diaries: Vec<ShortDiaryRecord>,
    pub(crate) existing: Vec<ExistingMemoryRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OrganizedOutput {
    #[serde(default)]
    pub(crate) knowledge: Vec<KnowledgeAction>,
    #[serde(default)]
    pub(crate) long_diaries: Vec<LongDiaryDraft>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct KnowledgeAction {
    pub(crate) operation: String,
    #[serde(default)]
    pub(crate) target_id: Option<i64>,
    pub(crate) memory_type: String,
    pub(crate) content: String,
    pub(crate) truth_status: String,
    pub(crate) importance: i64,
    pub(crate) confidence: f64,
    #[serde(default)]
    pub(crate) visibility: String,
    #[serde(default)]
    pub(crate) subjects: Vec<MemorySubject>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) diary_ids: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LongDiaryDraft {
    pub(crate) content: String,
    pub(crate) importance: i64,
    pub(crate) confidence: f64,
    #[serde(default)]
    pub(crate) visibility: String,
    #[serde(default)]
    pub(crate) subjects: Vec<MemorySubject>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) diary_ids: Vec<i64>,
}

