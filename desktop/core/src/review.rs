//! Local review commands. Sampling remains opt-in and separate from delivery.
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, String>;
#[derive(Serialize)]
pub struct ReviewSettings {
    pub revision: String,
    pub enabled: bool,
    pub daily_limit: u64,
}

const RETENTION: u64 = 7 * 86400;
#[derive(Serialize)]
pub struct ReviewItem {
    pub revision: String,
    pub id: String,
    pub created_at: u64,
    pub duration_ms: u64,
    pub asr_text: String,
    pub output_text: String,
    pub status: String,
    pub corrected_text: Option<String>,
    pub suggested_rule: Option<RuleSuggestion>,
    pub promotion_pending: bool,
}

#[derive(Serialize)]
pub struct RuleSuggestion {
    pub wrong: String,
    pub right: String,
}

pub struct ReviewStore {
    config: PathBuf,
    root: PathBuf,
}
impl ReviewStore {
    pub fn open(config: PathBuf, root: PathBuf) -> Result<Self> {
        if !config.is_absolute() || !root.is_absolute() {
            return Err("校對資料位置必須是完整路徑".into());
        }
        Ok(Self { config, root })
    }
    /// Native caller supplies wall-clock time; the webview cannot select it.
    pub fn list(&self, now: u64) -> Result<Vec<ReviewItem>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let _lock = self.lock()?;
        self.list_unlocked(now)
    }
    fn list_unlocked(&self, now: u64) -> Result<Vec<ReviewItem>> {
        let mut items = Vec::new();
        for (index, entry) in fs::read_dir(&self.root).map_err(io_error)?.enumerate() {
            if index >= 512 {
                return Err("校對目錄超過掃描限制，請先整理資料".into());
            }
            let entry = entry.map_err(io_error)?;
            let id = entry.file_name().to_string_lossy().into_owned();
            let abandoned = id.strip_prefix(".pending-").is_some_and(valid_id);
            if (!valid_id(&id) && !abandoned) || !entry.file_type().map_err(io_error)?.is_dir() {
                continue;
            }
            if let Ok((item, _)) = self.read_item(&id) {
                if now.saturating_sub(item.created_at) >= RETENTION {
                    fs::remove_dir_all(entry.path()).map_err(io_error)?;
                } else {
                    items.push(item);
                }
            } else if entry
                .metadata()
                .map_err(io_error)?
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|age| now.saturating_sub(age.as_secs()) >= RETENTION)
            {
                // Damaged metadata must not retain audio indefinitely. Match
                // the collector's directory-age cleanup for abandoned writes.
                fs::remove_dir_all(entry.path()).map_err(io_error)?;
            }
        }
        items.sort_by_key(|item| std::cmp::Reverse(item.created_at));
        Ok(items)
    }
    pub fn audio(&self, id: &str, expected: &str, now: u64) -> Result<Vec<u8>> {
        let _lock = self.lock()?;
        let (item, _) = self.current_item(id, expected, now)?;
        let data = read_optional(&self.path(id)?.join("audio.wav"), 61 * 16000 * 2 + 44)?
            .ok_or("這段音訊已清理")?;
        if data.len() < 44 {
            return Err("校對音訊格式無效".into());
        }
        let u32_at = |i| u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as u64;
        if &data[..4] != b"RIFF"
            || &data[8..16] != b"WAVEfmt "
            || u32_at(16) != 16
            || data[20..24] != [1, 0, 1, 0]
            || u32_at(24) != 16000
            || u32_at(28) != 32000
            || data[32..36] != [2, 0, 16, 0]
            || &data[36..40] != b"data"
            || u32_at(4) + 8 != data.len() as u64
            || u32_at(40) + 44 != data.len() as u64
            || u32_at(40) % 2 != 0
            || (u32_at(40) / 2 * 1000 / 16000) != item.duration_ms
        {
            return Err("校對音訊格式無效".into());
        }
        Ok(data)
    }
    pub fn delete(&self, id: &str, expected: &str, now: u64) -> Result<()> {
        let _lock = self.lock()?;
        self.current_item(id, expected, now)?;
        fs::remove_dir_all(self.path(id)?).map_err(io_error)
    }
    pub fn review(
        &self,
        id: &str,
        expected: &str,
        corrected: String,
        now: u64,
    ) -> Result<ReviewItem> {
        if corrected.trim().is_empty()
            || corrected.chars().count() > 4096
            || corrected.contains('\0')
        {
            return Err("請填入 1–4096 字的校對結果".into());
        }
        let _lock = self.lock()?;
        let (item, mut value) = self.current_item(id, expected, now)?;
        if value["promotion"]["state"] == "pending" {
            return Err("此筆詞庫更新尚未完成，請先重試詞庫更新".into());
        }
        value["status"] = json!(if corrected == item.output_text {
            "correct"
        } else {
            "corrected"
        });
        value["corrected_text"] = json!(corrected);
        value["reviewed_at"] = json!(now);
        write_json(&self.path(id)?.join("record.json"), &value, 128 * 1024)?;
        Ok(self.read_item(id)?.0)
    }
    /// A separate explicit user action. Persist intent before changing the
    /// dictionary so interrupted promotion can be retried without duplicate rules.
    pub fn promote(
        &self,
        id: &str,
        expected: &str,
        now: u64,
        vocabulary_path: &Path,
    ) -> Result<ReviewItem> {
        let _lock = self.lock()?;
        let (item, mut value) = self.current_item(id, expected, now)?;
        let pair = item
            .suggested_rule
            .as_ref()
            .ok_or("這不是單一短詞修正，請在詞庫頁填入完整片語")?;
        if item.promotion_pending
            && (value["promotion"]["wrong"] != pair.wrong
                || value["promotion"]["right"] != pair.right)
        {
            return Err("尚未完成的詞庫意圖與校對結果不同，原資料保留".into());
        }
        if pair.wrong.to_lowercase() != pair.right.to_lowercase() {
            let policy = one_rule(pair)?;
            for other in self.list_unlocked(now)? {
                if other.id != id && other.status != "pending" {
                    let confirmed = other
                        .corrected_text
                        .as_deref()
                        .unwrap_or(&other.output_text);
                    if policy.apply_with_terms(confirmed).0 != confirmed {
                        return Err("其他已確認句子仍使用這個原寫法，請改用更完整的片語".into());
                    }
                }
            }
        }
        value["promotion"] = json!({"wrong":pair.wrong,"right":pair.right,"state":"pending"});
        write_json(&self.path(id)?.join("record.json"), &value, 128 * 1024)?;
        let mut vocabulary = crate::vocabulary::Vocabulary::open(vocabulary_path.to_owned())?;
        let saved = vocabulary.snapshot();
        if !saved.entries.iter().any(|e| {
            e.right == pair.right
                && e.wrong
                    .iter()
                    .any(|w| w.to_lowercase() == pair.wrong.to_lowercase())
        }) {
            vocabulary.put(
                &saved.revision,
                None,
                vec![pair.wrong.clone()],
                pair.right.clone(),
            )?;
        }
        value["promotion"]["state"] = json!("applied");
        // Also complete a recoverable intent left by the legacy review UI.
        if let Some(corrected) = item.corrected_text {
            value["status"] = json!(if corrected == item.output_text {
                "correct"
            } else {
                "corrected"
            });
            value["corrected_text"] = json!(corrected);
            value["reviewed_at"] = json!(now);
            value
                .as_object_mut()
                .unwrap()
                .remove("pending_corrected_text");
        }
        write_json(&self.path(id)?.join("record.json"), &value, 128 * 1024)?;
        Ok(self.read_item(id)?.0)
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err("校對編號無效".into());
        }
        let path = self.root.join(id);
        let meta = fs::symlink_metadata(&path).map_err(io_error)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("校對資料不能是連結或特殊檔案".into());
        }
        Ok(path)
    }
    fn lock(&self) -> Result<fs::File> {
        let meta = fs::symlink_metadata(&self.root).map_err(io_error)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("校對目錄無效".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        }
        lock_file(&self.root.join(".lock"))
    }
    fn read_item(&self, id: &str) -> Result<(ReviewItem, Value)> {
        let data = read_optional(&self.path(id)?.join("record.json"), 128 * 1024)?;
        let bytes = data.as_deref().ok_or("找不到校對記錄")?;
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| "校對資料格式無效".to_string())?;
        let text = |key: &str| -> Result<String> {
            let text = value[key].as_str().ok_or("校對文字格式無效")?;
            if text.chars().count() > 4096 {
                return Err("校對文字超過大小限制".into());
            }
            Ok(text.to_owned())
        };
        let created_at = value["created_at"]
            .as_u64()
            .filter(|n| *n <= 253402214400)
            .ok_or("校對時間無效")?;
        let duration_ms = value["duration_ms"]
            .as_u64()
            .filter(|n| *n <= 61000)
            .ok_or("校對音訊格式無效")?;
        if value["version"] != 1 || value["id"] != id || value["sample_rate"] != 16000 {
            return Err("校對資料版本或格式無效".into());
        }
        let status = value["status"]
            .as_str()
            .filter(|s| ["pending", "correct", "corrected"].contains(s))
            .ok_or("校對狀態無效")?
            .to_owned();
        let corrected_text = if !value["pending_corrected_text"].is_null() {
            Some(text("pending_corrected_text")?)
        } else if value["corrected_text"].is_null() {
            None
        } else {
            Some(text("corrected_text")?)
        };
        Ok((
            ReviewItem {
                revision: revision(&data),
                id: id.into(),
                created_at,
                duration_ms,
                asr_text: text("asr_text")?,
                output_text: text("output_text")?,
                status,
                suggested_rule: corrected_text
                    .as_deref()
                    .and_then(|after| propose(&text("output_text").ok()?, after)),
                promotion_pending: value["promotion"]["state"] == "pending",
                corrected_text,
            },
            value,
        ))
    }
    fn current_item(&self, id: &str, expected: &str, now: u64) -> Result<(ReviewItem, Value)> {
        let (item, value) = self.read_item(id)?;
        if item.revision != expected || now.saturating_sub(item.created_at) >= RETENTION {
            return Err("校對資料已變更或到期，請重新載入".into());
        }
        Ok((item, value))
    }
    pub fn settings(&self) -> Result<ReviewSettings> {
        let data = read_optional(&self.config, 4096)?;
        settings(&data)
    }
    pub fn set_enabled(&self, expected: &str, enabled: bool) -> Result<ReviewSettings> {
        let dir = self.config.parent().ok_or("校對設定位置無效")?;
        fs::create_dir_all(dir).map_err(io_error)?;
        let _lock = lock_file(&self.config.with_extension("json.lock"))?;
        let data = read_optional(&self.config, 4096)?;
        let current = settings(&data)?;
        if current.revision != expected {
            return Err("抽樣設定已變更，請重新載入".into());
        }
        let mut value = config_value(&data)?;
        value["enabled"] = json!(enabled);
        write_json(&self.config, &value, 4096)?;
        self.settings()
    }
}
fn one_rule(pair: &RuleSuggestion) -> Result<voicetype_text::vocab::VocabSnapshot> {
    let source = toml::to_string(&json!({"entry":[{"wrong":[pair.wrong],"right":pair.right}]}))
        .map_err(|_| "無法預覽這筆規則".to_string())?;
    voicetype_text::vocab::VocabSnapshot::from_toml(&source).map_err(|_| "無法預覽這筆規則".into())
}

/// Conservative single-block token difference. English identifiers remain whole;
/// common internal tokens cause rejection instead of proposing multiple edits.
fn propose(before: &str, after: &str) -> Option<RuleSuggestion> {
    if before == after || before.contains(['`', '\n', '\r']) || after.contains(['`', '\n', '\r']) {
        return None;
    }
    fn tokens(text: &str) -> Vec<&str> {
        let mut output = Vec::new();
        let mut start = 0;
        while start < text.len() {
            let first = text[start..].chars().next().expect("nonempty suffix");
            let kind = |c: char| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    1
                } else if c.is_whitespace() {
                    2
                } else {
                    0
                }
            };
            let class = kind(first);
            let mut end = start + first.len_utf8();
            if class != 0 {
                for c in text[end..].chars() {
                    if kind(c) != class {
                        break;
                    }
                    end += c.len_utf8();
                }
            }
            output.push(&text[start..end]);
            start = end;
        }
        output
    }
    let old = tokens(before);
    let new = tokens(after);
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_mid = &old[prefix..old.len() - suffix];
    let new_mid = &new[prefix..new.len() - suffix];
    if old_mid.is_empty() || new_mid.is_empty() || old_mid.iter().any(|word| new_mid.contains(word))
    {
        return None;
    }
    let mut wrong = old_mid.concat();
    let mut right = new_mid.concat();
    let lowercase = |s: &str| {
        s.is_ascii()
            && s.chars().any(|c| c.is_ascii_lowercase())
            && !s.chars().any(|c| c.is_ascii_uppercase())
    };
    if (lowercase(&wrong) && lowercase(&right))
        || (!wrong.is_ascii() && wrong.chars().count() < 4)
        || wrong.chars().count() < 2
        || right.chars().count() < 2
    {
        let preceding = old[..prefix].concat();
        let mut context = Vec::new();
        for c in preceding.chars().rev().take(8) {
            if "，。！？；：,.!?;:\n\r\"'".contains(c) {
                break;
            }
            context.push(c);
            if context.len() >= 3 && (c.is_whitespace() || !c.is_ascii()) {
                break;
            }
        }
        let context: String = context.into_iter().rev().collect();
        if context.trim().is_empty() {
            return None;
        }
        wrong = context.clone() + &wrong;
        right = context + &right;
    }
    let pair = RuleSuggestion {
        wrong: wrong.trim().into(),
        right: right.trim().into(),
    };
    if [&pair.wrong, &pair.right].iter().any(|s| {
        !(2..=64).contains(&s.chars().count())
            || s.contains([
                '\n', '\r', '`', '/', '\\', '=', '<', '>', '[', ']', '{', '}',
            ])
    }) {
        return None;
    }
    (one_rule(&pair).ok()?.apply_with_terms(before).0 == after).then_some(pair)
}

fn valid_id(id: &str) -> bool {
    let parts: Vec<_> = id.split('-').collect();
    id.len() < 100
        && parts.len() == 4
        && parts[0] == "r"
        && parts[1..]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}
fn config_value(data: &Option<Vec<u8>>) -> Result<Value> {
    let value = match data {
        Some(data) => serde_json::from_slice::<Value>(data)
            .map_err(|_| "抽樣設定格式無效，原檔保留".to_string())?,
        None => json!({"enabled":false,"daily_limit":5}),
    };
    if value["enabled"].as_bool().is_none()
        || !value["daily_limit"]
            .as_u64()
            .is_some_and(|n| (1..=5).contains(&n))
    {
        return Err("抽樣設定格式無效，原檔保留".into());
    }
    Ok(value)
}
fn settings(data: &Option<Vec<u8>>) -> Result<ReviewSettings> {
    let value = config_value(data)?;
    Ok(ReviewSettings {
        revision: revision(data),
        enabled: value["enabled"].as_bool().unwrap(),
        daily_limit: value["daily_limit"].as_u64().unwrap(),
    })
}
fn revision(data: &Option<Vec<u8>>) -> String {
    let mut hash = Sha256::new();
    hash.update([u8::from(data.is_some())]);
    hash.update(data.as_deref().unwrap_or_default());
    format!("{:x}", hash.finalize())
}
fn io_error(_: std::io::Error) -> String {
    "無法存取校對資料，請檢查權限及可用空間".into()
}
fn regular(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            Err("校對資料不能是連結或特殊檔案".into())
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}
fn read_optional(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    regular(path)?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    let mut data = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(io_error)?;
    if data.len() > limit {
        return Err("校對資料超過大小限制".into());
    }
    Ok(Some(data))
}
fn lock_file(path: &Path) -> Result<fs::File> {
    regular(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(io_error)?;
    file.try_lock()
        .map_err(|_| "正在保存校對資料，請稍後再試".to_string())?;
    Ok(file)
}
fn write_json(path: &Path, value: &Value, limit: usize) -> Result<()> {
    regular(path)?;
    let dir = path.parent().ok_or("校對資料位置無效")?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(io_error)?;
    let data = serde_json::to_vec_pretty(value).map_err(|_| "校對資料格式無效".to_string())?;
    if data.len() + 1 > limit {
        return Err("校對資料超過大小限制，原檔保留".into());
    }
    temp.write_all(&data)
        .and_then(|_| temp.write_all(b"\n"))
        .map_err(io_error)?;
    temp.as_file().sync_all().map_err(io_error)?;
    temp.persist(path).map_err(|e| io_error(e.error))?;
    #[cfg(unix)]
    fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(io_error)?;
    Ok(())
}
