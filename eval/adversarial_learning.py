#!/usr/bin/env python3
"""Known role-collision regression through the actual isolated daemon text path.

These are synthetic text controls and one previously observed ASR snapshot.
They are never included in recording CER or called unseen generalization.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path

import replay_learning as replay


def controls(fixture):
    target = fixture['target_case']
    correct = target['reference']
    context = target['context']
    items = []
    for label, surrounding in [('empty', ''), ('provided', context),
                               ('irrelevant', '今天先整理餐點與交通安排。')]:
        items.append({'id': f'correct_mixed_roles_{label}', 'text': correct,
                      'context': surrounding, 'invariant': 'preserve_all'})
    for label, surrounding in [('empty', ''), ('provided', context)]:
        items.append({'id': f'known_p076_raw_{label}', 'text': target['raw_asr'],
                      'context': surrounding, 'invariant': 'preserve_first_role'})
    items.extend([
        {'id': 'two_distinct_names', 'text': '林志豪和林智豪兩位都會出席。',
         'context': context, 'invariant': 'preserve_all'},
        {'id': 'source_name_explicit_in_context', 'text': '窗口叫林志豪，稍後我會寄信給他。',
         'context': '行政窗口林志豪，請沿用名單上的原名。', 'invariant': 'preserve_all'},
        {'id': 'source_name_in_code_literal', 'text': '請保留 `林志豪` 這個識別字。',
         'context': '', 'invariant': 'preserve_all'},
        {'id': 'positive_teacher_with_support', 'text': '稍後請林志豪老師幫忙看這份摘要。',
         'context': '摘要由林智豪老師指導，主題是視覺定位。',
         'invariant': 'apply_single_supported_teacher'},
    ])
    return items


def passes(item, baseline, output, fixture):
    if item['invariant'] == 'preserve_all':
        return output == baseline
    if item['invariant'] == 'preserve_first_role':
        # p076's first clause was already correct; teachers may remain unresolved.
        # A same-CER repair elsewhere cannot excuse changing this correct role.
        return output.split('，', 1)[0] == baseline.split('，', 1)[0]
    if item['invariant'] == 'apply_single_supported_teacher':
        rule = fixture['causal_rule']
        return output == baseline.replace(rule['wrong'], rule['right'])
    raise ValueError('unknown invariant')


def run_mode(binary, directory, vocab, fixture, mode):
    client = replay.ReplayClient(binary, directory, vocab)
    items = controls(fixture)
    baseline = {}
    rows = []
    training = []
    session = 1000

    def render(text, context):
        nonlocal session
        session += 1
        request = replay.process_message(text, session, context, 'editor-main')
        response = client.request(request)
        if response.get('type') != 'replay_result' or response.get('session') != session:
            raise RuntimeError(f'bad process response: {response}')
        return request, response['text']

    try:
        assert client.rules() == []
        for item in items:
            _, baseline[item['id']] = render(item['text'], item['context'])
        source = fixture['training_case']
        for _ in range(1 if mode == 'confirmed' else 2):
            request, before = render(source['raw_asr'], source['context'])
            response = client.request(replay.correction_message(
                request['replay_session'], request, before, source['reference'], mode == 'confirmed'))
            training.append({'before': before, 'after': source['reference'], 'response': response})
        rules = client.rules()
        wanted = fixture['causal_rule']
        active = any(r.get('active') and r.get('wrong') == wanted['wrong']
                     and r.get('right') == wanted['right'] for r in rules)
        if not active:
            raise RuntimeError('regression control did not activate the intended prior rule')
        for item in items:
            _, output = render(item['text'], item['context'])
            rows.append({**item, 'baseline': baseline[item['id']], 'output': output,
                         'passed': passes(item, baseline[item['id']], output, fixture),
                         'unchanged': output == baseline[item['id']]})
        return {'learning': mode, 'training': training, 'rules': rules, 'checks': rows,
                'passed': sum(x['passed'] for x in rows),
                'failed': sum(not x['passed'] for x in rows)}
    finally:
        client.close()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--daemon-bin', type=Path, required=True)
    ap.add_argument('--fixture', type=Path, required=True)
    ap.add_argument('--vocab', type=Path, required=True)
    ap.add_argument('--output', type=Path, required=True, help='New private directory')
    args = ap.parse_args()
    os.umask(0o077)
    fixture = json.loads(args.fixture.read_text())
    if fixture['target_case']['id'] != 'p076':
        ap.error('this known regression suite expects the original p076 fixture')
    args.output.mkdir(parents=True, exist_ok=False)
    vocab = args.output/'vocab.toml'
    vocab.write_bytes(args.vocab.read_bytes())
    report = {'schema_version': 1, 'status': 'incomplete',
              'scope': 'Known role-collision text regression and synthetic controls; not ASR accuracy, unseen generalization, Fcitx capture or real user feedback.',
              'fixture_sha256': replay.scoring.artifact_digest(args.fixture),
              'binary_sha256': replay.scoring.artifact_digest(args.daemon_bin),
              'vocab_sha256': replay.scoring.artifact_digest(vocab),
              'harness_sha256': replay.scoring.artifact_digest(__file__),
              'modes': {}, 'errors': []}
    try:
        with open('/tmp/voicetype-evaluation.lock', 'a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            for mode in ['observed', 'confirmed']:
                report['modes'][mode] = run_mode(args.daemon_bin, args.output/mode, vocab, fixture, mode)
                replay.save_json(args.output/'report.json', report)
        report['status'] = 'complete'
    except Exception as exc:
        report['errors'].append(str(exc))
        raise
    finally:
        replay.save_json(args.output/'report.json', report)
    failed = sum(m['failed'] for m in report['modes'].values())
    print(json.dumps({'failed': failed, 'report': str(args.output/'report.json')}))
    return 3 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
