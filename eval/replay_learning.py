#!/usr/bin/env python3
"""Chronological correction replay through voicetyped's isolated real-runtime CLI.

Models/processing never receive references before returning the scored utterance.
Only dev feedback may train; previously examined test recordings are regression.
"""
import argparse
from collections import Counter
import fcntl
import hashlib
import itertools
import json
import os
from pathlib import Path
import selectors
import subprocess
import time

import evaluate_personal as scoring
import numeric_equivalence as numeric
from record_personal import load_corpus

ACCEPTED = {'pending', 'activated', 'confirmed', 'already_known'}
PROGRAM = 'adaptive-replay-editor'


def save_json(path, data):
    Path(path).write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')


def context_map(cases, mode):
    """Use only frozen context fields; never inspect references/terms to pick context."""
    if mode == 'empty':
        return {c['id']: ('', None) for c in cases}
    if mode == 'provided':
        return {c['id']: (c.get('context', ''), c['id']) for c in cases}
    if mode != 'mismatched':
        raise ValueError('unknown context mode')
    result = {}
    for i, case in enumerate(cases):
        donors = [cases[(i - offset) % len(cases)] for offset in range(1, len(cases))]
        donor = next((d for d in donors if d.get('context') and d['category'] != case['category']), None)
        result[case['id']] = ((donor['context'], donor['id']) if donor else ('另外一份文件正在討論餐點與交通安排。', None))
    return result


def process_message(text, session, context_text, context_id, program=PROGRAM):
    # Intentionally no case object or reference argument at the render seam.
    return {'type': 'process_text', 'text': scoring.TAG.sub('', text).strip(),
            'replay_session': session, 'program': program, 'context_id': context_id,
            'context_text': context_text, 'selected_text': '', 'mode': 'off'}


def correction_message(session, scope, before, human_after, confirmed):
    return {'type': 'correction', 'session': session, 'program': scope['program'],
            'context_id': scope['context_id'], 'before': before, 'after': human_after,
            'confirmed': confirmed}


class ReplayClient:
    def __init__(self, binary, run_dir, vocab, timeout=10):
        self.run_dir = Path(run_dir)
        self.run_dir.mkdir(parents=True, exist_ok=False)
        self.timeout = timeout
        self.stderr = (self.run_dir / 'daemon.stderr.log').open('wb')
        self.trace = (self.run_dir / 'events.jsonl').open('w')
        self.events = 0
        self.buffer = bytearray()
        self.learning_file = self.run_dir / 'learning.json'
        assert not self.learning_file.exists()
        env = {k: v for k, v in os.environ.items() if not k.startswith('VOICETYPE_')}
        env.update(VOICETYPE_LEARNING_FILE=str(self.learning_file.resolve()),
                   VOICETYPE_VOCAB=str(Path(vocab).resolve()), VOICETYPE_LOG='warn',
                   CUDA_VISIBLE_DEVICES='-1', HIP_VISIBLE_DEVICES='-1',
                   ROCR_VISIBLE_DEVICES='-1', VK_DRIVER_FILES='/dev/null')
        self.proc = subprocess.Popen([str(Path(binary).resolve()), '--replay-jsonl', '-'],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=self.stderr, env=env)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.proc.stdout, selectors.EVENT_READ)

    def audit(self, kind, payload):
        self.events += 1
        record = {'sequence': self.events, 'kind': kind, 'payload': payload}
        self.trace.write(json.dumps(record, ensure_ascii=False) + '\n')
        self.trace.flush()
        return self.events

    def request(self, message):
        self.audit('request', message)
        self.proc.stdin.write(json.dumps(message, ensure_ascii=False).encode() + b'\n')
        self.proc.stdin.flush()
        deadline = time.monotonic() + self.timeout
        while b'\n' not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not self.selector.select(remaining):
                raise TimeoutError('isolated replay daemon response timed out')
            chunk = os.read(self.proc.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError('isolated replay daemon disconnected; see daemon.stderr.log')
            self.buffer.extend(chunk)
            if len(self.buffer) > 1024 * 1024:
                raise RuntimeError('oversized replay response')
        line, _, rest = self.buffer.partition(b'\n')
        self.buffer = bytearray(rest)
        response = json.loads(line)
        self.audit('response', response)
        return response

    def rules(self):
        result = self.request({'type': 'list_learned'})
        if result.get('type') != 'replay_info' or not isinstance(result.get('value'), list):
            raise RuntimeError(f'wrong list_learned response: {result}')
        return result['value']

    def close(self):
        if self.proc.stdin:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.selector.close()
        self.proc.stdout.close()
        self.stderr.close()
        self.trace.close()
        if self.proc.returncode:
            raise RuntimeError(f'replay daemon exit {self.proc.returncode}; see {self.run_dir}')


def render_then_feedback(client, case, raw, session, context, context_id, learning, train):
    """One irreversible ordering: render → persist score → optional human feedback."""
    request = process_message(raw, session, context, context_id)
    started = time.perf_counter()
    response = client.request(request)
    elapsed = time.perf_counter() - started
    if response.get('type') != 'replay_result' or response.get('session') != session:
        raise RuntimeError(f'wrong replay_result: {response}')
    output = response['text']
    row = scoring.score_case(case, output)
    row.update(process_request_seconds=elapsed, supplied_context=context, context_id=context_id,
               source_raw_asr=raw, feedback_status='not_requested', feedback=None)
    row['pre_feedback_score_event'] = client.audit('score_before_feedback', {
        'id': case['id'], 'session': session, 'output': output,
        'character_errors': row['character_errors'], 'reference': case['reference']})
    # References enter the daemon only here, after this output has been scored.
    if train and learning != 'none':
        if scoring.scoring.normalize(output) == scoring.scoring.normalize(case['reference']):
            row['feedback_status'] = 'not_needed'
        else:
            message = correction_message(session, request, output, case['reference'], learning == 'confirmed')
            feedback = client.request(message)
            row['feedback'] = feedback
            if feedback.get('type') == 'replay_correction':
                row['feedback_status'] = feedback['outcome']['status']
            elif feedback.get('type') == 'replay_error':
                row['feedback_status'] = 'attribution_or_runtime_error'
            else:
                raise RuntimeError(f'wrong correction response: {feedback}')
            row['feedback_response_event'] = client.events
            assert row['pre_feedback_score_event'] < row['feedback_response_event']
    return row


def summaries(rows):
    result = {}
    for label, selected in [('full_known_regression', rows),
                            ('dev_chronological', [r for r in rows if r['split'] == 'dev']),
                            ('known_test_regression', [r for r in rows if r['split'] == 'test'])]:
        if selected:
            result[label] = {'literal': scoring.aggregate(selected), 'numeric_equivalent': numeric.supplemental(selected),
                             'categories': {cat: scoring.aggregate([r for r in selected if r['category'] == cat])
                                            for cat in sorted({r['category'] for r in selected})}}
    return result


def paired(candidate, baseline):
    previous = {r['id']: r for r in baseline}
    assert set(previous) == {r['id'] for r in candidate}
    literal_improved, literal_worsened, unchanged_errors, changed_equal_errors = [], [], [], []
    correct_broken, changed, negative_changes, negative_worsened = [], [], [], []
    for row in candidate:
        old = previous[row['id']]
        delta = row['character_errors'] - old['character_errors']
        (literal_improved if delta < 0 else literal_worsened if delta > 0 else unchanged_errors).append(row['id'])
        if row['text'] != old['text']:
            changed.append(row['id'])
            if delta == 0:
                changed_equal_errors.append(row['id'])
            if row['category'] == 'negative_controls':
                negative_changes.append(row['id'])
        if old['character_errors'] == 0 and row['character_errors'] > 0:
            correct_broken.append(row['id'])
        if row['category'] == 'negative_controls' and delta > 0:
            negative_worsened.append(row['id'])
    old_error = sum(r['character_errors'] for r in baseline)
    new_error = sum(r['character_errors'] for r in candidate)
    old_num, new_num = numeric.supplemental(baseline), numeric.supplemental(candidate)
    return {'improved_ids': literal_improved, 'worsened_ids': literal_worsened,
            'equal_error_ids': unchanged_errors, 'changed_equal_error_ids': changed_equal_errors,
            'changed_ids': changed, 'previously_correct_broken_ids': correct_broken,
            'negative_control_changed_ids': negative_changes, 'negative_control_worsened_ids': negative_worsened,
            'literal_relative_error_reduction_percent': 100 * (old_error-new_error)/old_error if old_error else None,
            'numeric_relative_error_reduction_percent': 100 * (old_num['errors']-new_num['errors'])/old_num['errors'] if old_num['errors'] else None,
            'definition': 'Worsening is additional character errors vs identical runtime/context/scope without learning; equal-error rewrites and negative controls remain manual-review items, not automatically safe.'}


def run_condition(binary, cases, raw, vocab, directory, context_mode, scope_mode, learning, timeout):
    client = ReplayClient(binary, directory, vocab, timeout)
    rows = []
    contexts = context_map(cases, context_mode)
    try:
        assert client.rules() == [], 'replay must start with empty isolated memory'
        # dev first, then known regression: test gold never trains the store.
        ordered = [c for split in ['dev', 'test'] for c in cases if c['split'] == split]
        for number, case in enumerate(ordered, 1):
            context, donor = contexts[case['id']]
            ic = 'editor-main' if scope_mode == 'stable' else f"field-{case['id']}"
            row = render_then_feedback(client, case, raw[case['id']]['raw_output'], number,
                                       context, ic, learning, case['split'] == 'dev')
            row['context_source_id'] = donor
            rows.append(row)
        rules = client.rules()
    finally:
        client.close()
    counts = Counter(r['feedback_status'] for r in rows)
    return {'status': 'complete', 'learning': learning, 'context_mode': context_mode, 'scope_mode': scope_mode,
            'cases': rows, 'scores': summaries(rows), 'final_rules': rules,
            'feedback_counts': dict(counts),
            'accepted_feedback': sum(counts[s] for s in ACCEPTED),
            'refused_feedback': counts['rejected'] + counts['attribution_or_runtime_error'],
            'active_rules': sum(bool(r.get('active')) for r in rules),
            'notes': ['Every utterance scored before its optional human feedback. No same-case reprocessing after feedback.',
                      'Only dev corrections train. Previously inspected test cases are known regression, not unseen evidence.',
                      'All runs start with empty isolated memory; no existing user learning rules copied. Same frozen static vocabulary for every run.',
                      'Faithful/non-generative mode; screen helper/refiner absent. Context from fixed corpus fields, never reconstructed from answers.',
                      'Runtime CLI exercises real text pipeline and Assistant attribution/learning, not Fcitx capture, typing, or actual desktop notifications.']}


def protocol_probes(binary, directory, vocab, timeout):
    """Synthetic state transitions, separate from real-recording accuracy scores."""
    client = ReplayClient(binary, directory, vocab, timeout)
    checks = []

    def check(name, condition, observed):
        checks.append({'name': name, 'passed': bool(condition), 'observed': observed})

    def render(session, text='推到 zentak 上面', context='', ic='editor-main', program=PROGRAM):
        message = process_message(text, session, context, ic, program)
        result = client.request(message)
        if result.get('type') != 'replay_result':
            raise RuntimeError(f'probe render failed: {result}')
        return message, result['text']

    try:
        check('empty_initial_store', client.rules() == [], [])
        scope, before = render(1001)
        check('unlearned_word_preserved', before == '推到 zentak 上面', before)
        original = correction_message(1001, scope, before, '推到 zentek 上面', False)
        for field, wrong in [('session', 9999), ('program', 'other-app'),
                             ('context_id', 'other-field'), ('before', before + '額外文字')]:
            invalid = dict(original, **{field: wrong})
            outcome = client.request(invalid)
            check(f'attribution_refuses_wrong_{field}', outcome.get('type') == 'replay_error', outcome)
        check('refused_feedback_does_not_change_store', client.rules() == [], client.rules())
        first = client.request(original)
        check('first_observation_pending', first.get('outcome', {}).get('status') == 'pending', first)
        duplicate = client.request(original)
        check('same_session_not_double_counted', duplicate.get('outcome', {}).get('status') == 'duplicate', duplicate)
        scope2, before2 = render(1002)
        check('one_observation_does_not_apply', before2 == '推到 zentak 上面', before2)
        second = client.request(correction_message(1002, scope2, before2, '推到 zentek 上面', False))
        check('second_independent_observation_activates', second.get('outcome', {}).get('status') == 'activated', second)
        _, applied = render(1003)
        check('future_same_field_uses_prior_learning', applied == '推到 zentek 上面', applied)
        _, fresh = render(1004, ic='fresh-field')
        check('new_field_without_context_abstains', fresh == '推到 zentak 上面', fresh)
        _, supported = render(1005, context='專案代稱 zentek', ic='fresh-field')
        check('independent_right_context_allows_transfer', supported == '推到 zentek 上面', supported)
        _, misleading = render(1006, context='此處必須保留 zentak 這個原名')
        check('context_naming_wrong_form_prevents_replacement', misleading == '推到 zentak 上面', misleading)
        _, other_app = render(1007, program='other-app')
        check('other_app_abstains', other_app == '推到 zentak 上面', other_app)
        literal = '保留 `zentak` 這個識別字'
        _, protected = render(1008, text=literal)
        check('code_literal_preserved', protected == literal, protected)
        unknown_scope, unknown_before = render(1009, program='')
        unknown = client.request(correction_message(1009, unknown_scope, unknown_before, '推到 zentek 上面', True))
        check('explicit_unknown_app_refused', unknown.get('outcome', {}).get('reason') == 'missing_context', unknown)
        result = {'status': 'complete', 'scope': 'Synthetic JSONL real-runtime attribution/learning; no Fcitx event or actual desktop notification claim.',
                  'checks': checks, 'passed': sum(c['passed'] for c in checks), 'failed': sum(not c['passed'] for c in checks),
                  'final_rules': client.rules()}
    finally:
        client.close()
    return result


def load_inputs(corpus_path, source_paths):
    corpus = load_corpus(corpus_path)
    by_id = {c['id']: c for c in corpus['cases']}
    raw = {}
    for source in source_paths:
        report = json.loads(Path(source).read_text())
        if report.get('status') != 'complete':
            raise ValueError(f'incomplete source: {source}')
        if report.get('corpus_sha256') != scoring.artifact_digest(corpus_path):
            raise ValueError(f'corpus hash differs: {source}')
        for row in report['cases']:
            cid = row['id']
            if cid in raw or cid not in by_id:
                raise ValueError(f'duplicate/unknown case: {cid}')
            if row['reference'] != by_id[cid]['reference'] or row['split'] != by_id[cid]['split']:
                raise ValueError(f'reference/split mismatch: {cid}')
            if scoring.artifact_digest(row['wav']) != row['wav_sha256']:
                raise ValueError(f'audio changed: {cid}')
            raw[cid] = row
    cases = [c for c in corpus['cases'] if c['id'] in raw]
    if len([c for c in cases if c['split'] == 'dev']) != 60:
        raise ValueError('the chronological learning phase requires all 60 dev cases')
    if len(cases) not in (60, 100):
        raise ValueError('provide all dev, optionally with all 40 known regression cases')
    return cases, raw


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--daemon-bin', type=Path, required=True)
    ap.add_argument('--raw', type=Path, action='append', required=True, help='Complete raw-ASR report; repeat for known test')
    ap.add_argument('--corpus', type=Path, default=Path(__file__).parent / 'fixtures/dictation-demo.json')
    ap.add_argument('--vocab', type=Path, required=True, help='Read-only static vocab.toml snapshot, not a personal learning file')
    ap.add_argument('--output', type=Path, required=True, help='New private report directory; must not exist')
    ap.add_argument('--contexts', default='empty,provided,mismatched')
    ap.add_argument('--scopes', default='stable,fresh')
    ap.add_argument('--learning', default='none,observed,confirmed')
    ap.add_argument('--timeout', type=float, default=10)
    args = ap.parse_args()
    os.umask(0o077)
    contexts, scopes, learning = (v.split(',') for v in (args.contexts, args.scopes, args.learning))
    if not set(contexts) <= {'empty', 'provided', 'mismatched'} or not set(scopes) <= {'stable','fresh'} or not set(learning) <= {'none','observed','confirmed'} or 'none' not in learning:
        ap.error('invalid matrix or missing no-learning control')
    cases, raw = load_inputs(args.corpus, args.raw)
    args.output.mkdir(parents=True, exist_ok=False)
    vocab_bytes = args.vocab.read_bytes()
    frozen_vocab = args.output / 'vocab.toml'
    frozen_vocab.write_bytes(vocab_bytes)
    report = {'schema_version': 1, 'status': 'incomplete',
              'daemon_binary_sha256': scoring.artifact_digest(args.daemon_bin),
              'corpus_sha256': scoring.artifact_digest(args.corpus),
              'vocab_sha256': hashlib.sha256(vocab_bytes).hexdigest(),
              'evaluation_sources': {str(p): scoring.artifact_digest(p) for p in [
                  Path(__file__), Path(scoring.__file__), Path(scoring.scoring.__file__),
                  Path(numeric.__file__)]},
              'raw_sources': {str(p): scoring.artifact_digest(p) for p in args.raw},
              'conditions': {}, 'errors': [], 'working_relative_improvement_target_percent': 20,
              'primary_condition': 'observed-stable-empty',
              'primary_scope': 'Chronological dev then frozen-memory known regression; not unseen generalization or Fcitx desktop capture evidence.'}
    try:
        # Text timing only. Inference/performance agent owns ASR cold/warm tests.
        with open('/tmp/voicetype-evaluation.lock', 'a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            report['protocol_probes'] = protocol_probes(args.daemon_bin, args.output/'protocol-probes', frozen_vocab, args.timeout)
            save_json(args.output / 'report.json', report)
            for context_mode, scope_mode, feedback in itertools.product(contexts, scopes, learning):
                key = f'{feedback}-{scope_mode}-{context_mode}'
                print(key, flush=True)
                result = run_condition(args.daemon_bin, cases, raw, frozen_vocab, args.output/key,
                                       context_mode, scope_mode, feedback, args.timeout)
                report['conditions'][key] = result
                save_json(args.output / 'report.json', report)
        for key, result in report['conditions'].items():
            baseline = report['conditions'][f"none-{result['scope_mode']}-{result['context_mode']}"]
            result['paired_against_no_learning'] = paired(result['cases'], baseline['cases'])
            full = result['scores']['full_known_regression']['literal']
            gain = result['paired_against_no_learning']['numeric_relative_error_reduction_percent']
            result['working_gates'] = {'full_100_available': len(result['cases']) == 100,
                                      'literal_cer_under_10': full['cer_percent'] < 10,
                                      'numeric_relative_reduction_at_least_20': gain is not None and gain >= 20,
                                      'no_newly_broken_correct_utterances': not result['paired_against_no_learning']['previously_correct_broken_ids'],
                                      'no_negative_control_rewrite_needing_review': not result['paired_against_no_learning']['negative_control_changed_ids'],
                                      'not_a_goal_completion_claim': True}
        if args.vocab.read_bytes() != vocab_bytes:
            raise RuntimeError('source static vocabulary changed while evaluating')
        report['status'] = 'complete'
    except Exception as exc:
        report['errors'].append(str(exc))
        raise
    finally:
        save_json(args.output / 'report.json', report)
    return 3 if report['protocol_probes']['failed'] else 0


if __name__ == '__main__':
    raise SystemExit(main())
