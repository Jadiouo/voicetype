"""Selection helper privacy/attribution tests; no desktop access or user text."""
import importlib.util
from pathlib import Path
import types
import unittest
from unittest.mock import patch
from contextlib import ExitStack

PATH = Path(__file__).resolve().parents[2] / 'scripts/context-selection.py'
spec = importlib.util.spec_from_file_location('selection', PATH)
selection = importlib.util.module_from_spec(spec)
spec.loader.exec_module(selection)


class States(set):
    def contains(self, value): return value in self


class Node:
    def __init__(self, role='frame', states=(), children=(), text=None, span=(7, 13), pid=42):
        self.role, self.states, self.children = role, States(states), list(children)
        self.text, self.span, self.pid = text, span, pid
        self.path = '/accessible/field'
        self.app = types.SimpleNamespace(bus_name=':1.999')
        self.reads = []
        self.pending_states = None
    def clear_cache(self):
        if self.pending_states is not None:
            self.states, self.pending_states = self.pending_states, None
    def get_role(self): return self.role
    def get_state_set(self): return self.states
    def get_child_count(self): return len(self.children)
    def get_child_at_index(self, i): return self.children[i]
    def get_process_id(self): return self.pid
    def get_text_iface(self): return self if self.text is not None else None
    def get_name(self): raise AssertionError('accessible names must never be read')
    def get_attributes(self): return {}  # targeted AX enable only; result discarded
    def get_relation_set(self): return []


class Text:
    @staticmethod
    def get_n_selections(node): return 1 if node.span else 0
    @staticmethod
    def get_selection(node, _):
        return types.SimpleNamespace(start_offset=node.span[0], end_offset=node.span[1])
    @staticmethod
    def get_character_count(node): return len(node.text)
    @staticmethod
    def get_text(node, start, end):
        node.reads.append((start, end))
        if getattr(node, 'lose_focus_after_read', False):
            node.pending_states = States(node.states - {'focused'})
        return node.text[start:end]


API = types.SimpleNamespace(Text=Text,
    StateType=types.SimpleNamespace(ACTIVE='active', SHOWING='showing', FOCUSED='focused',
                                   EDITABLE='editable', ENABLED='enabled'),
    Role=types.SimpleNamespace(PASSWORD_TEXT='password', ENTRY='entry', TEXT='text'))


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.field = Node('entry', ('showing', 'focused', 'editable', 'enabled'),
                          text='prefix GitHub suffix')
        self.secret_label = Node('label', ('showing',), text='unrelated page content')
        self.frame = Node(states=('active', 'showing'), children=[self.secret_label, self.field])
        self.app = Node(children=[self.frame])
        self.desktop = Node(children=[self.app])
    def capture(self, **kwargs):
        return selection.capture('fixture', API, self.desktop,
            kwargs.pop('window_reader', lambda: ('0x123', 42)),
            kwargs.pop('matcher', lambda program, pid: True), **kwargs)
    def reject(self, code, **kwargs):
        with self.assertRaisesRegex(selection.Rejected, '^' + code + '$'):
            self.capture(**kwargs)
    def collection(self, matches):
        stack = ExitStack()
        self.frame.get_collection_iface = lambda: self.frame
        def query(frame, rule, sort, count, traverse):
            self.assertIs(frame, self.frame)
            self.assertEqual(count, 2)
            self.assertEqual(rule, ['focused', 'editable'])
            return matches
        for name, value in {
            'StateSet': types.SimpleNamespace(new=lambda states: states),
            'MatchRule': types.SimpleNamespace(new=lambda states, *args: states),
            'CollectionMatchType': types.SimpleNamespace(ALL='all'),
            'CollectionSortOrder': types.SimpleNamespace(CANONICAL='canonical'),
            'Collection': types.SimpleNamespace(get_matches=query),
        }.items():
            stack.enter_context(patch.object(API, name, value, create=True))
        return stack
    def test_collection_skips_large_unrelated_page_without_reading(self):
        self.frame.children = [self.secret_label] * 1000 + [self.field]
        with self.collection([self.field]):
            self.assertEqual(self.capture()['text'], 'GitHub')
        self.assertEqual(self.secret_label.reads, [])
    def test_collection_rejects_password_and_competing_unknown_role(self):
        with self.collection([self.field, Node('document')]):
            self.reject('ambiguous_focus')
        self.field.role = 'password'
        with self.collection([self.field]):
            self.reject('sensitive_field')
        self.assertEqual(self.field.reads, [])
    def test_collection_candidate_requires_fresh_focus(self):
        self.field.lose_focus_after_read = True
        with self.collection([self.field]):
            self.reject('focus_changed')
    def test_runtime_request_only_target_frame_then_retry(self):
        self.frame.children = []
        calls = []
        def activate():
            calls.append('target_frame')
            self.frame.children = [self.field]
            return {}
        self.frame.get_attributes = activate
        self.assertEqual(self.capture()['text'], 'GitHub')
        self.assertEqual(calls, ['target_frame'])
    def test_reads_only_selected_range_twice_no_page_or_name(self):
        result = self.capture()
        self.assertEqual(result['text'], 'GitHub')
        self.assertEqual(self.field.reads, [(7, 13), (7, 13)])
        self.assertEqual(self.secret_label.reads, [])
        self.assertNotIn('suffix', str(result))
    def test_mismatched_program_does_not_read(self):
        self.reject('program_pid_mismatch', matcher=lambda *_: False)
        self.assertEqual(self.field.reads, [])
    def test_mismatched_app_pid_never_searches_by_name(self):
        self.app.pid = 43
        self.reject('accessibility_unavailable')
        self.assertEqual(self.field.reads, [])
    def test_multiple_focused_editables_rejected_before_read(self):
        self.frame.children.append(Node('entry', self.field.states, text='other'))
        self.reject('ambiguous_focus')
        self.assertEqual(self.field.reads, [])
    def test_password_never_reads_text(self):
        self.field.role = 'password'
        self.reject('sensitive_field')
        self.assertEqual(self.field.reads, [])
    def test_unknown_editable_role_rejected(self):
        self.field.role = 'document'
        self.reject('unsupported_editable_role')
    def test_noneditable_selection_not_accepted(self):
        self.field.states.remove('editable')
        self.reject('no_focused_editable')
    def test_absent_selection_is_actionable(self):
        self.field.span = None
        self.reject('no_selection')
    def test_oversized_selection_rejected_before_read(self):
        self.field.text, self.field.span = 'x' * 513, (0, 513)
        self.reject('selection_too_long')
        self.assertEqual(self.field.reads, [])
    def test_focus_changed_discards_selected_text(self):
        windows = iter([('0x123', 42), ('0x456', 42)])
        self.reject('focus_changed', window_reader=lambda: next(windows))
    def test_cached_focus_must_refresh_before_accepting(self):
        self.field.lose_focus_after_read = True
        self.reject('no_focused_editable')
        self.assertEqual(self.field.reads, [(7, 13)])
    def test_deadline_is_bounded(self):
        self.reject('timeout', budget=-1)
        self.assertEqual(self.field.reads, [])
    def test_ambiguous_window_is_not_guessed(self):
        self.app.children.append(self.frame)
        self.reject('ambiguous_active_window')


if __name__ == '__main__':
    unittest.main()
