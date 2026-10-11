//! Lossless, revision-checked vocabulary commands. The UI never chooses a path.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Value};
use voicetype_text::vocab::VocabSnapshot;

const MAX_BYTES: usize = 128 * 1024;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub wrong: Vec<String>,
    pub right: String,
}
#[derive(Default, Deserialize)]
struct Definition {
    #[serde(default)]
    entry: Vec<Entry>,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    terms: Vec<String>,
}
#[derive(Serialize)]
pub struct VocabularyView {
    pub revision: String,
    pub entries: Vec<Entry>,
    pub names: Vec<String>,
    pub terms: Vec<String>,
    pub name_conversion_available: bool,
}

pub struct Vocabulary {
    path: PathBuf,
    original: Option<Vec<u8>>,
    document: DocumentMut,
    definition: Definition,
    policy: VocabSnapshot,
}
impl Vocabulary {
    pub fn open(path: PathBuf) -> Result<Self> {
        let original = read(&path)?;
        let (document, definition, policy) = parse(original.as_deref().unwrap_or_default())?;
        Ok(Self {
            path,
            original,
            document,
            definition,
            policy,
        })
    }
    pub fn snapshot(&self) -> VocabularyView {
        VocabularyView {
            revision: revision(&self.original),
            entries: self.definition.entry.clone(),
            names: self.definition.names.clone(),
            terms: self.definition.terms.clone(),
            name_conversion_available: voicetype_text::Traditional::load().is_some(),
        }
    }
    /// Preview exact vocabulary policy on supplied text, with code/name guards.
    /// No recording, inference, generative rewrite or learning is performed.
    pub fn preview(&self, text: &str) -> Result<String> {
        if text.len() > 16 * 1024 || text.contains('\0') {
            return Err("預覽文字過長或含無效字元".into());
        }
        Ok(self.policy.apply_with_terms(text).0)
    }
    pub fn put(
        &mut self,
        expected: &str,
        index: Option<usize>,
        wrong: Vec<String>,
        right: String,
    ) -> Result<VocabularyView> {
        let mut next = self.document.clone();
        if !next.contains_key("entry") {
            next["entry"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
        }
        let wrong: toml_edit::Array = wrong.into_iter().collect();
        if let Some(tables) = next["entry"].as_array_of_tables_mut() {
            let table = match index {
                Some(i) => tables.get_mut(i).ok_or("詞彙不存在，請重新載入")?,
                None => {
                    tables.push(toml_edit::Table::new());
                    let last = tables.len() - 1;
                    tables.get_mut(last).unwrap()
                }
            };
            replace(&mut table["wrong"], Value::Array(wrong));
            replace(&mut table["right"], Value::from(right));
        } else if let Some(array) = next["entry"].as_array_mut() {
            let table = match index {
                Some(i) => array
                    .get_mut(i)
                    .and_then(Value::as_inline_table_mut)
                    .ok_or("詞彙不存在，請重新載入")?,
                None => {
                    array.push(toml_edit::InlineTable::new());
                    let last = array.len() - 1;
                    array.get_mut(last).unwrap().as_inline_table_mut().unwrap()
                }
            };
            table.insert("wrong", Value::Array(wrong));
            table.insert("right", Value::from(right));
        } else {
            return Err("詞庫格式無效，原檔已保留".into());
        }
        self.write(expected, next.to_string().into_bytes())
    }
    pub fn set_names(&mut self, expected: &str, names: Vec<String>) -> Result<VocabularyView> {
        let mut next = self.document.clone();
        replace(
            &mut next["names"],
            Value::Array(names.into_iter().collect()),
        );
        self.write(expected, next.to_string().into_bytes())
    }
    pub fn set_terms(&mut self, expected: &str, terms: Vec<String>) -> Result<VocabularyView> {
        let mut next = self.document.clone();
        replace(
            &mut next["terms"],
            Value::Array(terms.into_iter().collect()),
        );
        self.write(expected, next.to_string().into_bytes())
    }
    /// Explicit one-time copy. Existing daily data is never modified, and an
    /// already-created app vocabulary must be edited rather than overwritten.
    pub fn import_existing(&mut self, expected: &str, source: &Path) -> Result<VocabularyView> {
        if self.original.is_some() {
            return Err("App 已有詞庫，不能以匯入覆蓋；請直接編輯".into());
        }
        let data = read(source)?.ok_or("找不到原有詞庫")?;
        self.write(expected, data)
    }
    pub fn delete(&mut self, expected: &str, index: usize) -> Result<VocabularyView> {
        if index >= self.definition.entry.len() {
            return Err("詞彙不存在，請重新載入".into());
        }
        let mut next = self.document.clone();
        if let Some(tables) = next["entry"].as_array_of_tables_mut() {
            tables.remove(index);
        } else if let Some(array) = next["entry"].as_array_mut() {
            array.remove(index);
        }
        self.write(expected, next.to_string().into_bytes())
    }
    pub fn restore(&mut self, expected: &str) -> Result<VocabularyView> {
        let data = read(&self.path.with_extension("toml.bak"))?.ok_or("目前沒有上次儲存的備份")?;
        self.write(expected, data)
    }
    fn write(&mut self, expected: &str, data: Vec<u8>) -> Result<VocabularyView> {
        let (document, definition, policy) = parse(&data)?;
        let dir = self.path.parent().ok_or("詞庫位置無效")?;
        fs::create_dir_all(dir).map_err(io_error)?;
        let lock_path = self.path.with_extension("toml.lock");
        reject_symlink(&lock_path)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(lock_path).map_err(io_error)?;
        lock.try_lock()
            .map_err(|_| "另一個程式正在儲存詞庫，請稍後再試".to_string())?;
        self.check_revision(expected)?;
        if let Some(previous) = &self.original {
            atomic_write(&self.path.with_extension("toml.bak"), previous, None)?;
        }
        self.check_revision(expected)?;
        let permissions = fs::metadata(&self.path).ok().map(|m| m.permissions());
        atomic_write(&self.path, &data, permissions)?;
        self.original = Some(data);
        self.document = document;
        self.definition = definition;
        self.policy = policy;
        Ok(self.snapshot())
    }
    fn check_revision(&self, expected: &str) -> Result<()> {
        if expected != revision(&self.original) || read(&self.path)? != self.original {
            return Err("詞庫已在其他地方修改，請重新載入後再儲存".into());
        }
        Ok(())
    }
}
fn replace(item: &mut Item, mut value: Value) {
    if let Some(old) = item.as_value() {
        *value.decor_mut() = old.decor().clone();
    }
    *item = Item::Value(value);
}
fn parse(data: &[u8]) -> Result<(DocumentMut, Definition, VocabSnapshot)> {
    if data.len() > MAX_BYTES {
        return Err("詞庫超過 128 KiB，原檔已保留".into());
    }
    let text = std::str::from_utf8(data).map_err(|_| "詞庫必須使用 UTF-8".to_string())?;
    let definition: Definition =
        toml::from_str(text).map_err(|_| "詞庫格式無效，原檔已保留".to_string())?;
    let valid = |s: &str, min: usize| {
        (min..=64).contains(&s.chars().count())
            && !s.trim().is_empty()
            && !s.chars().any(|c| c < ' ' || c == '\u{7f}')
    };
    if !definition.names.iter().all(|s| valid(s, 2))
        || !definition.terms.iter().all(|s| valid(s, 1))
        || !definition.entry.iter().all(|e| {
            valid(&e.right, 1) && !e.wrong.is_empty() && e.wrong.iter().all(|s| valid(s, 1))
        })
    {
        return Err(
            "詞彙需 1–64 字，名字需 2–64 字；不能只有空白、含控制字元或沒有辨識錯字".into(),
        );
    }
    let policy =
        VocabSnapshot::from_toml(text).map_err(|e| format!("詞庫無法套用，原檔已保留：{e}"))?;
    let document = text
        .parse()
        .map_err(|_| "詞庫格式無效，原檔已保留".to_string())?;
    Ok((document, definition, policy))
}
fn revision(data: &Option<Vec<u8>>) -> String {
    let mut hash = Sha256::new();
    hash.update([u8::from(data.is_some())]);
    hash.update(data.as_deref().unwrap_or_default());
    format!("{:x}", hash.finalize())
}
fn io_error(_: std::io::Error) -> String {
    "無法存取詞庫，請檢查權限及可用空間".into()
}
fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            Err("詞庫必須是一般檔案，未修改連結或其他檔案".into())
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    reject_symlink(path)?;
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    let mut data = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .map_err(io_error)?;
    if data.len() > MAX_BYTES {
        return Err("詞庫超過 128 KiB，原檔已保留".into());
    }
    Ok(Some(data))
}
fn atomic_write(path: &Path, data: &[u8], permissions: Option<fs::Permissions>) -> Result<()> {
    reject_symlink(path)?;
    let dir = path.parent().ok_or("詞庫位置無效")?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(io_error)?;
    if let Some(permissions) = permissions {
        temp.as_file()
            .set_permissions(permissions)
            .map_err(io_error)?;
    }
    temp.write_all(data).map_err(io_error)?;
    temp.as_file().sync_all().map_err(io_error)?;
    temp.persist(path).map_err(|e| io_error(e.error))?;
    #[cfg(unix)]
    fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(io_error)?;
    Ok(())
}
