#!/usr/bin/env python3
"""CPU-only character spelling correction; output is validated positional edits.

Never reconstruct a transcript from token decoding. The model sees a simplified
shadow, while accepted edits are applied individually to the original string.
"""
import re
import time

MIN_PROBABILITY = 0.98
MAX_ORIGINAL_PROBABILITY = 0.01
MAX_CHARS = 1024
CHUNK_CHARS = 96
CONTEXT_CHARS = 16
MAX_TOKENS = 512
PROTECTED_CHARS = set(
    "不沒没未無无非勿莫別别零〇一二兩两三四五六七八九十百千萬万億亿兆幾几"
    "我你妳您他她它牠祂咱俺買买賣卖"
)
LITERAL_CUES = ("字面", "拼法", "拼寫", "變數名稱", "變數名", "不要改", "保留原樣", "刻意取")
TITLE = re.compile(r"[\u4e00-\u9fff]{2,3}(?=教授|老師|先生|女士|小姐|醫師|博士|主任|經理|同學)")
PATH = re.compile(r"(?:[A-Za-z]:)?[/\\][^\s，。；、！？]+|\b\w+://[^\s，。；、！？]+")
FILENAME = re.compile(r"[\w.-]+\.[A-Za-z0-9]{1,12}\b")


def han(ch):
    return len(ch) == 1 and "\u4e00" <= ch <= "\u9fff"


def protected_positions(text, terms=()):
    protected = set()
    # Matching delimiter runs, including unterminated literals. Backticks within
    # a differently sized run cannot prematurely end an outer code fence.
    ticks, begin = 0, 0
    for match in re.finditer(r"`+", text):
        count = len(match.group())
        if not ticks:
            ticks, begin = count, match.start()
        elif ticks == count:
            protected.update(range(begin, match.end()))
            ticks = 0
    if ticks:
        protected.update(range(begin, len(text)))
    # Quoted text is a literal, including single quotes. An apostrophe in an
    # English contraction or possessive must not open a quote over later Han.
    opening = {'「': '」', '『': '』', '“': '”', '"': '"', '‘': '’', "'": "'"}
    quote, begin = None, 0
    for index, ch in enumerate(text):
        if index in protected:  # A quote inside a backtick fence is inert.
            continue
        before = index > 0 and text[index-1].isascii() and text[index-1].isalnum()
        after = index+1 < len(text) and text[index+1].isascii() and text[index+1].isalnum()
        if quote:
            if ch == quote and not (ch in ("'", "’") and before and after):
                protected.update(range(begin, index+1))
                quote = None
        elif ch in opening and not (ch in ("'", "‘") and before):
            quote, begin = opening[ch], index
    if quote:
        protected.update(range(begin, len(text)))
    for pattern in (PATH, FILENAME, TITLE):
        for match in pattern.finditer(text):
            protected.update(range(match.start(), match.end()))
    for term in terms:
        if not isinstance(term, str) or not term or len(term) > 64:
            continue
        # Canonical terms have already been supplied by the explicit dictionary
        # or personal learning. Do not let a generic model undo them.
        for match in re.finditer(re.escape(term), text, re.IGNORECASE):
            protected.update(range(match.start(), match.end()))
    for match in re.finditer(r"[^。！？；，,\n]+[。！？；，,\n]?", text):
        if any(cue in match.group() for cue in LITERAL_CUES):
            protected.update(range(match.start(), match.end()))
    protected.update(i for i, ch in enumerate(text) if ch in PROTECTED_CHARS)
    return protected


def guard_edits(text, proposals, terms=()):
    from pypinyin import pinyin, Style
    protected = protected_positions(text, terms)
    accepted = []
    occupied = set()
    for edit in proposals:
        start, source, target = edit["start"], edit["source"], edit["target"]
        if (not isinstance(start, int) or start < 0 or start >= len(text)
                or start in occupied or start in protected or text[start] != source
                or not han(source) or not han(target) or source == target
                or target in PROTECTED_CHARS):
            continue
        if not (MIN_PROBABILITY <= edit["probability"] <= 1
                and 0 <= edit["original_probability"] <= MAX_ORIGINAL_PROBABILITY):
            continue
        # Same syllable, ignoring tone, is a prerequisite for this ASR policy.
        # Similar spelling is insufficient: e.g. 錄音 -> 錄影 changes the meaning.
        before = set(pinyin(source, style=Style.NORMAL, heteronym=True)[0])
        after = set(pinyin(target, style=Style.NORMAL, heteronym=True)[0])
        if not before.intersection(after):
            continue
        occupied.add(start)
        accepted.append(dict(start=start, source=source, target=target))
    # A high edit density suggests rewriting or an input/model mismatch.
    if len(accepted) > max(2, sum(han(ch) for ch in text) // 10):
        return []
    return sorted(accepted, key=lambda edit: edit["start"])


def apply_edits(text, edits):
    result = list(text)
    for edit in edits:
        assert result[edit["start"]] == edit["source"]
        result[edit["start"]] = edit["target"]
    return "".join(result)


class Engine:
    def __init__(self, model, tokenizer, threads=4):
        import numpy as np
        import onnxruntime as ort
        from opencc import OpenCC
        from tokenizers import Tokenizer
        self.np = np
        self.ort = ort
        options = ort.SessionOptions()
        options.intra_op_num_threads = threads
        options.inter_op_num_threads = 1
        options.add_session_config_entry("session.intra_op.allow_spinning", "0")
        options.add_session_config_entry("session.inter_op.allow_spinning", "0")
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        self.session = ort.InferenceSession(str(model), sess_options=options,
                                            providers=["CPUExecutionProvider"])
        if self.session.get_providers() != ["CPUExecutionProvider"]:
            raise RuntimeError("CPU-only provider required")
        self.tokenizer = Tokenizer.from_file(str(tokenizer))
        self.to_simple, self.to_trad = OpenCC("t2s"), OpenCC("s2tw")

    def predict(self, text, terms=(), budget_ms=80):
        started = time.monotonic()
        self.last_calls = 0
        if len(text) > MAX_CHARS or not any(han(ch) for ch in text):
            return [], "skipped"
        simple = self.to_simple.convert(text)
        if len(simple) != len(text):
            return [], "alignment_skip"
        proposals = []
        protected = protected_positions(text, terms)
        # Each output position belongs to exactly one core. Context overlaps do
        # not duplicate edits, trim the original, or leave an unprocessed tail.
        for core_start in range(0, len(text), CHUNK_CHARS):
            core_end = min(core_start + CHUNK_CHARS, len(text))
            if all(i in protected or not han(text[i]) for i in range(core_start, core_end)):
                continue
            if (time.monotonic() - started) * 1000 >= budget_ms:
                return [], "deadline"
            start, end = max(0, core_start-CONTEXT_CHARS), min(len(text), core_end+CONTEXT_CHARS)
            chunk = simple[start:end]
            encoded = self.tokenizer.encode(chunk)
            if len(encoded.ids) > MAX_TOKENS:
                return [], "alignment_skip"
            arrays = dict(input_ids=encoded.ids, attention_mask=encoded.attention_mask,
                          token_type_ids=encoded.type_ids)
            inputs = {item.name: self.np.asarray([arrays[item.name]], dtype=self.np.int64)
                      for item in self.session.get_inputs()}
            logits = self.session.run(None, inputs)[0][0]
            self.last_calls += 1
            local = []
            predicted = list(chunk)
            for i, (left, right) in enumerate(encoded.offsets):
                absolute = start + left
                if (right-left != 1 or not core_start <= absolute < core_end
                        or absolute in protected or not han(chunk[left:right])):
                    continue
                winner = int(self.np.argmax(logits[i]))
                target = self.tokenizer.id_to_token(winner)
                if not han(target) or target == chunk[left:right]:
                    continue
                shifted = logits[i] - float(logits[i].max())
                normalizer = float(self.np.exp(shifted).sum())
                probability = 1.0 / normalizer
                original_probability = float(self.np.exp(shifted[encoded.ids[i]])) / normalizer
                if probability < MIN_PROBABILITY or original_probability > MAX_ORIGINAL_PROBABILITY:
                    continue
                predicted[left] = target
                local.append(dict(start=absolute, source=text[absolute], simple_offset=left,
                                  probability=probability, original_probability=original_probability))
            # Phrase-aware s2tw chooses 髮/發 etc.; only proposed positions are
            # copied back, so all unrelated original Traditional forms survive.
            traditional = self.to_trad.convert("".join(predicted))
            if len(traditional) != len(chunk):
                return [], "alignment_skip"
            for edit in local:
                edit["target"] = traditional[edit.pop("simple_offset")]
                proposals.append(edit)
        edits = guard_edits(text, proposals, terms)
        if (time.monotonic() - started) * 1000 >= budget_ms:
            return [], "deadline"
        if not self.last_calls:
            return [], "skipped"
        return edits, "applied" if edits else "unchanged"


def handle_request(engine, request):
    """Same validated request/age budget for socket and owned-pipe transports."""
    text, terms = request["text"], request.get("terms", [])
    if (type(request.get("v")) is not int or request["v"] != 1
            or type(request.get("id")) is not int
            or not 0 <= request["id"] < 2**64
            or not isinstance(text, str) or len(text) > 4096
            or not isinstance(terms, list) or len(terms) > 256
            or any(not isinstance(t, str) or not 0 < len(t) <= 64 for t in terms)
            or type(request.get("sent_at_ms")) is not int):
        raise ValueError("invalid request")
    age = max(0, time.time()*1000-request["sent_at_ms"])
    budget = min(80, 95-age)
    if budget <= 0:
        edits, status, calls = [], "deadline", 0
    else:
        edits, status = engine.predict(text, terms, budget_ms=budget)
        calls = engine.last_calls
    reply = dict(v=1, id=request["id"], status=status, edits=edits, model_calls=calls)
    return reply


def serve_stdio(engine):
    """Private parent-owned pipe. EOF exits; malformed frames fail closed.

    stdout is protocol-only UTF-8 bytes on every platform, including Windows.
    There is no listening port, inherited socket, per-request process or log of
    transcript content. The owner must stop waiting at its unchanged deadline.
    """
    import json
    import sys
    output = sys.stdout.buffer

    def send(reply):
        output.write(json.dumps(reply, ensure_ascii=False).encode("utf-8") + b"\n")
        output.flush()

    send(dict(v=1, status="ready", provider="CPUExecutionProvider"))
    while True:
        line = sys.stdin.buffer.readline(65537)
        if not line:
            return
        try:
            if len(line) > 65536 or not line.endswith(b"\n"):
                raise ValueError("invalid request length")
            request = json.loads(line)
            send(handle_request(engine, request))
        except BrokenPipeError:
            return
        except Exception:
            # A corrupt frame cannot be safely associated with a caller's id.
            # The parent observes EOF and retains its original text.
            print("spelling pipe request failed", file=sys.stderr, flush=True)
            raise SystemExit(2) from None


def serve(engine, socket_path):
    """One bounded request per local connection. No text is logged or retained."""
    import json
    import os
    import signal
    import socket
    from pathlib import Path

    path = Path(socket_path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if path.parent.stat().st_uid != os.getuid() or path.parent.stat().st_mode & 0o077:
        raise RuntimeError("Spelling socket requires an owned private directory")
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(path))  # Do not unlink another process's existing socket.
    path.chmod(0o600)
    inode = path.stat().st_ino
    listener.listen(2)
    listener.settimeout(1)
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    print(json.dumps({"status": "ready", "provider": "CPUExecutionProvider"}), flush=True)
    try:
        while not stopping:
            try:
                connection, _ = listener.accept()
            except socket.timeout:
                continue
            with connection:
                connection.settimeout(.05)
                started = time.monotonic()
                try:
                    data = bytearray()
                    while b"\n" not in data:
                        remaining = .05-(time.monotonic()-started)
                        if remaining <= 0:
                            raise TimeoutError("request deadline")
                        connection.settimeout(remaining)
                        chunk = connection.recv(4096)
                        if not chunk or len(data) + len(chunk) > 65536:
                            raise ValueError("invalid request length")
                        data.extend(chunk)
                    request = json.loads(data.split(b"\n", 1)[0])
                    reply = handle_request(engine, request)
                    status, edits, calls = reply["status"], reply["edits"], reply["model_calls"]
                    connection.sendall(json.dumps(reply,ensure_ascii=False).encode()+b"\n")
                    print(json.dumps({"status":status, "edits":len(edits), "model_calls":calls,
                                      "elapsed_ms":round((time.monotonic()-started)*1000,3)}),flush=True)
                except (ValueError, KeyError, TypeError, OSError, UnicodeError):
                    # Closing the connection makes the client retain its input.
                    print('{"status":"request_failed"}', flush=True)
                except Exception as error:
                    print(json.dumps({"status":"inference_failed", "error_type":type(error).__name__}),flush=True)
    finally:
        listener.close()
        if path.exists() and path.stat().st_ino == inode:
            path.unlink()


def main():
    import argparse
    import hashlib
    from pathlib import Path

    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", required=True, type=Path)
    ap.add_argument("--tokenizer", required=True, type=Path)
    ap.add_argument("--model-sha256", required=True)
    ap.add_argument("--tokenizer-sha256", required=True)
    transport = ap.add_mutually_exclusive_group(required=True)
    transport.add_argument("--socket", type=Path)
    transport.add_argument("--stdio", action="store_true", help="Use private parent-owned JSONL pipes")
    ap.add_argument("--threads", type=int, default=4, choices=[1,2,4,8])
    args = ap.parse_args()
    for path, expected in [(args.model,args.model_sha256),(args.tokenizer,args.tokenizer_sha256)]:
        with path.open("rb") as stream:
            if hashlib.file_digest(stream,"sha256").hexdigest() != expected:
                raise SystemExit("Model/tokenizer hash mismatch")
    engine = Engine(args.model,args.tokenizer,args.threads)
    engine.predict("今天心情很好。",budget_ms=1000)
    if args.stdio:
        serve_stdio(engine)
    else:
        serve(engine,args.socket)


if __name__ == "__main__":
    main()
