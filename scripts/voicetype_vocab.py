"""Lossless vocabulary editing. No microphone, model, or daemon lifecycle calls."""
import copy
import ctypes
import ctypes.util
import os
from pathlib import Path
import stat
import tempfile
import tomllib

import tomlkit
from tomlkit.items import AoT

MAX_BYTES = 128 * 1024


def name_aliases(names):
    """Account for the automatic OpenCC aliases also created by the daemon."""
    if not names:
        return []
    library = ctypes.util.find_library("opencc")
    if not library:
        raise ValueError("缺少 OpenCC，無法驗證名字保護設定。")
    api = ctypes.CDLL(library)
    api.opencc_open.argtypes = [ctypes.c_char_p]
    api.opencc_open.restype = ctypes.c_void_p
    api.opencc_close.argtypes = [ctypes.c_void_p]
    api.opencc_convert_utf8.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t]
    api.opencc_convert_utf8.restype = ctypes.c_void_p
    api.opencc_convert_utf8_free.argtypes = [ctypes.c_void_p]
    api.opencc_convert_utf8_free.restype = None
    handle = api.opencc_open(b"s2tw.json")
    if not handle or handle == ctypes.c_void_p(-1).value:
        raise ValueError("OpenCC s2tw 資料無法載入，未修改詞庫。")
    variants = []
    try:
        for name in sorted(set(names)):
            encoded = name.encode("utf-8")
            pointer = api.opencc_convert_utf8(handle, encoded, len(encoded))
            if not pointer:
                raise ValueError("OpenCC 無法驗證名字，未修改詞庫。")
            try:
                variants.append((ctypes.string_at(pointer).decode("utf-8"), name))
            finally:
                api.opencc_convert_utf8_free(pointer)
    finally:
        api.opencc_close(handle)
    return [(variant, name) for variant, name in variants
            if variant != name and variant not in names
            and sum(value == variant for value, _ in variants) == 1]


def vocabulary_path():
    return Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "voicetype/vocab.toml"


def read_optional(path):
    try:
        with path.open("rb") as file:
            data = file.read(MAX_BYTES + 1)
        if len(data) > MAX_BYTES:
            raise ValueError("詞庫超過 128 KiB，請先縮小檔案。")
        return data
    except FileNotFoundError:
        return None


def validate(data):
    """Match daemon limits; additionally disallow control characters in terms."""
    if len(data) > MAX_BYTES:
        raise ValueError("詞庫超過 128 KiB。")
    value = tomllib.loads(data.decode("utf-8"))

    def term(text, minimum=1):
        if not isinstance(text, str) or not minimum <= len(text) <= 64:
            raise ValueError(f"每個詞必須有 {minimum}–64 個字。")
        if any(ord(c) < 32 or ord(c) == 127 for c in text) or not text.strip():
            raise ValueError("詞彙不能是空白或含換行、控制字元。")

    for key in ("names", "terms"):
        items = value.get(key, [])
        if not isinstance(items, list) or len(items) > 256:
            raise ValueError(f"{key} 必須是清單，最多 256 個詞。")
        for item in items:
            term(item, 2 if key == "names" else 1)
    entries = value.get("entry", [])
    if not isinstance(entries, list) or len(entries) > 1024:
        raise ValueError("最多可儲存 1024 筆詞彙。")
    derived = name_aliases(value.get("names", []))
    aliases = {}
    for wrong, right in derived:
        key = wrong.lower()
        if key in aliases and aliases[key] != right:
            raise ValueError(f"名字的繁體別名「{wrong}」有大小寫衝突，請只保留一個正確寫法。")
        aliases[key] = right
    count = len(derived)
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("詞彙格式錯誤。")
        right = entry.get("right")
        term(right)
        wrongs = entry.get("wrong")
        if not isinstance(wrongs, list) or not wrongs:
            raise ValueError("每筆至少填一個辨識錯字。")
        for wrong in wrongs:
            term(wrong)
            key = wrong.lower()
            if key in aliases and aliases[key] != right:
                raise ValueError(f"「{wrong}」已對應「{aliases[key]}」，不能同時改成「{right}」。")
            aliases[key] = right
            count += 1
    if count > 4096:
        raise ValueError("所有辨識錯字合計不可超過 4096 個。")
    return value


def atomic_write(path, data, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix=f".{path.name}-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as file:
            os.fchmod(file.fileno(), mode)
            file.write(data)
            file.flush()
            os.fsync(file.fileno())
        os.replace(name, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(name):
            os.unlink(name)


class Vocabulary:
    def __init__(self, path=None):
        self.path = Path(path) if path is not None else vocabulary_path()
        self.backup = self.path.with_name(self.path.name + ".bak")
        self.reload()

    def reload(self):
        original = read_optional(self.path)
        validate(original or b"")
        document = tomlkit.parse((original or b"").decode("utf-8"))
        self.original, self.document = original, document

    @property
    def entries(self):
        return self.document.get("entry", [])

    @property
    def names(self):
        return list(self.document.get("names", []))

    def save(self, document):
        data = tomlkit.dumps(document).encode("utf-8")
        validate(data)
        self._write(data)

    def _write(self, data):
        # Other editors must not be silently overwritten by a stale GUI.
        if read_optional(self.path) != self.original:
            raise ValueError("詞庫已在其他地方修改。請先按「重新載入」，再重新編輯。")
        mode = stat.S_IMODE(self.path.stat().st_mode) if self.path.exists() else 0o600
        if self.original is not None:
            atomic_write(self.backup, self.original)
        if read_optional(self.path) != self.original:
            raise ValueError("儲存時詞庫有其他修改；本次未覆蓋。請重新載入。")
        atomic_write(self.path, data, mode)
        self.original = data
        self.document = tomlkit.parse(data.decode("utf-8"))

    def put(self, index, wrongs, right):
        document = copy.deepcopy(self.document)
        if "entry" not in document:
            document["entry"] = tomlkit.aot()
        if index is None:
            entry = tomlkit.table() if isinstance(document["entry"], AoT) else tomlkit.inline_table()
            document["entry"].append(entry)
            entry = document["entry"][-1]
        else:
            entry = document["entry"][index]
        entry["wrong"] = list(dict.fromkeys(wrongs))
        entry["right"] = right
        self.save(document)

    def delete(self, index):
        document = copy.deepcopy(self.document)
        del document["entry"][index]
        self.save(document)

    def set_names(self, names):
        document = copy.deepcopy(self.document)
        document["names"] = list(dict.fromkeys(names))
        self.save(document)

    def restore(self):
        data = read_optional(self.backup)
        if data is None:
            raise ValueError("目前沒有上次儲存的備份。")
        validate(data)
        self._write(data)
