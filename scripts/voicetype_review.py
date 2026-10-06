"""Private, bounded human review records, shared with the Rust collector."""
from contextlib import contextmanager
import difflib
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import time

from voicetype_vocab import Vocabulary, atomic_write

RETENTION = 7 * 86400
ID = re.compile(r"r-\d+-\d+-\d+\Z")


def config_path():
    return Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "voicetype/review.json"


def read_config():
    try:
        with config_path().open('rb') as file:
            data = file.read(4097)
        if len(data) > 4096:
            raise ValueError('抽樣設定過大。')
        value = json.loads(data)
        if not isinstance(value, dict):
            raise ValueError('抽樣設定格式錯誤。')
        if type(value.get("enabled")) is not bool or type(value.get("daily_limit")) is not int or not 1 <= value["daily_limit"] <= 5:
            raise ValueError("抽樣設定格式錯誤。")
        return value
    except FileNotFoundError:
        return {"enabled": False, "daily_limit": 5}


def set_enabled(enabled):
    atomic_write(config_path(), json.dumps({"enabled": bool(enabled), "daily_limit": 5}).encode() + b"\n")


def root_path():
    return Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "voicetype/review"


def json_bytes(value):
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode()


def read_record(path):
    with path.open("rb") as file:
        data = file.read(128 * 1024 + 1)
    if len(data) > 128 * 1024:
        raise ValueError("校對資料過大。")
    value = json.loads(data)
    if not isinstance(value, dict) or value.get("version") != 1 or value.get("id") != path.parent.name:
        raise ValueError("校對資料格式不正確。")
    for key in ("asr_text", "output_text"):
        if not isinstance(value.get(key), str) or len(value[key]) > 4096:
            raise ValueError("校對文字格式不正確。")
    if type(value.get("created_at")) is not int or not 0 <= value["created_at"] <= 253402214400:
        raise ValueError("校對時間不正確。")
    if type(value.get("duration_ms")) is not int or not 0 <= value["duration_ms"] <= 61000 or value.get("sample_rate") != 16000:
        raise ValueError("校對音訊格式不正確。")
    for key in ("corrected_text", "pending_corrected_text"):
        if value.get(key) is not None and (not isinstance(value[key], str) or len(value[key]) > 4096):
            raise ValueError("校對文字格式不正確。")
    if value.get("status") not in ("pending", "correct", "corrected"):
        raise ValueError("校對狀態不正確。")
    return value


def propose_pair(before, after):
    """A visible suggestion, never a learned rule until the user chooses it."""
    if not before or before == after or any(c in before + after for c in "`\n\r"):
        return None
    # Compare whole English words, otherwise GTHUB -> GitHub appears as several
    # character edits despite being a single corrected term.
    old_tokens = re.findall(r"[A-Za-z0-9_]+|\s+|.", before)
    new_tokens = re.findall(r"[A-Za-z0-9_]+|\s+|.", after)
    changes = [op for op in difflib.SequenceMatcher(None, old_tokens, new_tokens, autojunk=False).get_opcodes() if op[0] != "equal"]
    if len(changes) != 1:
        return None
    _, a, b, c, d = changes[0]
    start, end = len(''.join(old_tokens[:a])), len(''.join(old_tokens[:b]))
    new_start, new_end = len(''.join(new_tokens[:c])), len(''.join(new_tokens[:d]))
    latin = lambda c: c.isascii() and (c.isalnum() or c == '_')
    # Include the complete ASCII word rather than replacing an internal suffix.
    while start > 0 and new_start > 0 and latin(before[start - 1]) and latin(after[new_start - 1]):
        start -= 1; new_start -= 1
    while end < len(before) and new_end < len(after) and latin(before[end]) and latin(after[new_end]):
        end += 1; new_end += 1
    wrong, right = before[start:end], after[new_start:new_end]
    if not wrong or not right:
        return None
    # Lowercase English words and single Han characters require surrounding
    # phrase context. This avoids offering bare coming -> commit or 庫 -> 股.
    needs_context = (wrong.isascii() and wrong.islower() and right.islower()) or (not wrong.isascii() and len(wrong) < 4) or len(wrong) < 2 or len(right) < 2
    if needs_context:
        boundary = start
        while boundary > 0 and start - boundary < 8 and before[boundary - 1] not in "，。！？；：,.!?;:\n\r\"'":
            boundary -= 1
            if start - boundary >= 3 and (before[boundary].isspace() or not before[boundary].isascii()):
                break
        prefix = before[boundary:start]
        if not prefix.strip() or after[new_start - len(prefix):new_start] != prefix:
            return None
        wrong, right = prefix + wrong, prefix + right
    wrong, right = wrong.strip(), right.strip()
    if not 2 <= len(wrong) <= 64 or not 2 <= len(right) <= 64:
        return None
    if any(c in wrong + right for c in "\n\r`/\\=<>[]{}"):
        return None
    return wrong, right


def contains_alias(text, wrong):
    pattern = re.escape(wrong)
    if wrong[0].isascii() and (wrong[0].isalnum() or wrong[0] == '_'):
        pattern = r"(?<![A-Za-z0-9_])" + pattern
    if wrong[-1].isascii() and (wrong[-1].isalnum() or wrong[-1] == '_'):
        pattern += r"(?![A-Za-z0-9_])"
    return re.search(pattern, text, re.IGNORECASE) is not None


class ReviewStore:
    def __init__(self, root=None):
        self.root = Path(root) if root is not None else root_path()

    @contextmanager
    def locked(self):
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.root.is_symlink():
            raise ValueError("校對目錄不能是連結。")
        self.root.chmod(0o700)
        fd = os.open(self.root / '.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise ValueError("正在保存抽樣，請稍後再試。") from None
            yield
        finally:
            os.close(fd)

    def path(self, item_id):
        if not isinstance(item_id, str) or not ID.fullmatch(item_id):
            raise ValueError("無效的校對編號。")
        path = self.root / item_id
        if path.is_symlink():
            raise ValueError("校對資料不能是連結。")
        return path

    def _list(self, timestamp=None):
        timestamp = time.time() if timestamp is None else timestamp
        records = []
        for path in self.root.iterdir():
            if not ID.fullmatch(path.name) or path.is_symlink() or not path.is_dir():
                continue
            try:
                record = read_record(path / 'record.json')
                if timestamp - record['created_at'] >= RETENTION:
                    shutil.rmtree(path)
                else:
                    records.append(record)
            except (OSError, ValueError):
                # The Rust collector expires corrupt items by directory age.
                continue
        return sorted(records, key=lambda r: r['created_at'], reverse=True)

    def list(self):
        if not self.root.exists():
            return []
        with self.locked():
            return self._list()

    def audio_path(self, item_id):
        path = self.path(item_id) / 'audio.wav'
        if path.is_symlink() or not path.is_file():
            raise ValueError("這段音訊已清理或無法讀取。")
        return path

    def delete(self, item_id):
        with self.locked():
            shutil.rmtree(self.path(item_id))

    def review(self, item_id, corrected, *, expected_output, learn=False, vocabulary_path=None):
        if not isinstance(corrected, str) or not corrected.strip() or len(corrected) > 4096 or '\0' in corrected:
            raise ValueError("請填入 1–4096 字的校對結果。")
        with self.locked():
            timestamp = time.time()
            path = self.path(item_id) / 'record.json'
            record = read_record(path)
            if record['output_text'] != expected_output or timestamp - record['created_at'] >= RETENTION:
                raise ValueError("這筆資料已變更或到期，請重新整理。")
            pair = propose_pair(record['output_text'], corrected) if learn else None
            if learn and pair is None:
                raise ValueError("這不是單一短詞修正，請先記錄整句，或到詞庫頁自行新增。")
            if pair:
                wrong, right = pair
                if wrong.lower() != right.lower():
                    for other in self._list(timestamp):
                        if other['id'] != item_id and other['status'] != 'pending' and contains_alias(other['corrected_text'] or other['output_text'], wrong):
                            raise ValueError("已確認的其他句子仍使用這個原寫法。請先記錄整句，再用更完整的片語新增詞庫。")
                # Keep a recoverable intent before touching the second store.
                # A retry is idempotent if the dictionary committed first.
                record['promotion'] = {'wrong': wrong, 'right': right, 'state': 'pending'}
                record['pending_corrected_text'] = corrected
                atomic_write(path, json_bytes(record))
                vocabulary = Vocabulary(vocabulary_path)
                existing = {w.lower(): e['right'] for e in vocabulary.entries for w in e['wrong']}
                if existing.get(wrong.lower()) != right:
                    vocabulary.put(None, [wrong], right)
                record['promotion']['state'] = 'applied'
            record['corrected_text'] = corrected
            record['status'] = 'correct' if corrected == record['output_text'] else 'corrected'
            record['reviewed_at'] = int(time.time())
            record.pop('pending_corrected_text', None)
            atomic_write(path, json_bytes(record))
            return pair
