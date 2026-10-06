"""Chronological evaluation invariants; synthetic text, no microphone or production writes."""
import unittest
import evaluate_personal as scoring
import replay_learning as replay


def case(cid='p001', reference='推到 zentek 上面', category='code_switch', split='dev'):
    return {'id': cid, 'reference': reference, 'split': split, 'category': category,
            'context': '', 'terms': [], 'notes': 'never sent to renderer'}


class FakeRuntime:
    def __init__(self):
        self.events = 0
        self.sent = []
        self.audit_log = []
        self.learned = False

    def audit(self, kind, payload):
        self.events += 1
        self.audit_log.append((kind, payload))
        return self.events

    def request(self, message):
        self.sent.append(dict(message))
        self.audit('request', dict(message))
        if message['type'] == 'process_text':
            text = message['text'].replace('zentak', 'zentek') if self.learned else message['text']
            result = {'type': 'replay_result', 'session': message['replay_session'], 'text': text}
        elif message['type'] == 'correction':
            self.learned = True
            result = {'type': 'replay_correction', 'outcome': {'status': 'confirmed', 'changed': True}}
        else:
            raise AssertionError(message)
        self.audit('response', result)
        return result


class ReplayOrderTests(unittest.TestCase):
    def test_first_output_scored_before_feedback_and_only_next_can_improve(self):
        client = FakeRuntime()
        first = replay.render_then_feedback(client, case(), '推到 zentak 上面', 1, '', 'editor', 'confirmed', True)
        self.assertEqual(first['text'], '推到 zentak 上面')
        self.assertGreater(first['character_errors'], 0)
        self.assertEqual([m['type'] for m in client.sent], ['process_text', 'correction'])
        self.assertNotIn('zentek', str(client.sent[0]))
        self.assertNotIn('reference', client.sent[0])
        self.assertNotIn('terms', client.sent[0])
        audit_kinds = [kind for kind, _ in client.audit_log]
        self.assertLess(audit_kinds.index('score_before_feedback'), len(audit_kinds)-2)
        later = replay.render_then_feedback(client, case('p002'), '推到 zentak 上面', 2, '', 'editor', 'confirmed', True)
        self.assertEqual(later['character_errors'], 0)
        self.assertEqual(later['feedback_status'], 'not_needed')
        self.assertEqual(len(client.sent), 3)  # no rerender of the first case

    def test_known_regression_never_supplies_its_truth_as_training(self):
        client = FakeRuntime()
        row = replay.render_then_feedback(client, case(split='test'), '推到 zentak 上面', 1,
                                          '', 'editor', 'confirmed', False)
        self.assertGreater(row['character_errors'], 0)
        self.assertEqual(len(client.sent), 1)
        self.assertFalse(client.learned)

    def test_no_learning_control_and_tag_stripping_do_not_leak_answer(self):
        client = FakeRuntime()
        replay.render_then_feedback(client, case(), '<|zh|>推到 zentak 上面', 1, '', 'editor', 'none', True)
        self.assertEqual(client.sent[0]['text'], '推到 zentak 上面')
        self.assertEqual(len(client.sent), 1)
        self.assertEqual(client.sent[0]['selected_text'], '')

    def test_context_mismatch_uses_context_not_reference(self):
        cases = [{'id': 'a', 'category': 'a', 'context': '相機設定', 'reference': object()},
                 {'id': 'b', 'category': 'b', 'context': '交通安排', 'terms': object()}]
        self.assertEqual(replay.context_map(cases, 'empty'), {'a': ('', None), 'b': ('', None)})
        self.assertEqual(replay.context_map(cases, 'mismatched'), {'a': ('交通安排', 'b'), 'b': ('相機設定', 'a')})

    def test_new_errors_and_equal_error_rewrites_are_not_hidden(self):
        a = case('p001', '模型太大', 'negative_controls')
        b = case('p002', '先跑一次')
        old = [scoring.score_case(a, '模型太大'), scoring.score_case(b, '先跑二次')]
        new = [scoring.score_case(a, '模型台大'), scoring.score_case(b, '先跑三次')]
        metrics = replay.paired(new, old)
        self.assertEqual(metrics['previously_correct_broken_ids'], ['p001'])
        self.assertEqual(metrics['negative_control_worsened_ids'], ['p001'])
        self.assertEqual(metrics['changed_equal_error_ids'], ['p002'])
        self.assertLess(metrics['numeric_relative_error_reduction_percent'], 0)

    def test_numeric_rules_match_frozen_safety_examples(self):
        replay.numeric.self_check()


if __name__ == '__main__':
    unittest.main()
