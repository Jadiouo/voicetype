#!/usr/bin/python3
"""Explicit-shortcut helper: read only the focused editable field's selection.

Never read PRIMARY, clipboard, window titles, accessible names or page text.
The caller must still verify its delivered dictation/session and user intent.
stdout is a private pipe containing one bounded JSON reply, not a log.
"""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time

MAX_SELECTION = 512
MAX_NODES = 512


class Rejected(Exception):
    pass


def active_window():
    if not os.environ.get('DISPLAY'):
        raise Rejected('unsupported_display')
    def prop(*args):
        return subprocess.check_output(['xprop', *args], text=True,
            stderr=subprocess.DEVNULL, timeout=.08)
    result = re.search(r'0x[0-9a-fA-F]+', prop('-root', '_NET_ACTIVE_WINDOW'))
    if not result or int(result[0], 16) == 0:
        raise Rejected('missing_active_window')
    window = result[0]
    result = re.search(r'= (\d+)', prop('-id', window, '_NET_WM_PID'))
    if not result or int(result[1]) <= 0:
        raise Rejected('missing_window_pid')
    return window, int(result[1])


def program_matches(program, pid):
    # Fcitx's program and executable names differ for the Chrome launcher only.
    # Never search another process with a similar name if the PID does not match.
    aliases = {'google-chrome-stable': {'chrome'}, 'google-chrome': {'chrome'},
               'chromium-browser': {'chromium', 'chrome'}, 'chromium': {'chromium', 'chrome'}}
    if not program or Path(program).name != program:
        return False
    try:
        executable = (Path('/proc') / str(pid) / 'exe').resolve(strict=True).name
    except OSError:
        return False
    return executable in aliases.get(program, {program})


def focused_field(app, api, deadline):
    # Restrict traversal to active windows belonging to the exact X11 owner PID.
    app.clear_cache()
    if app.get_child_count() > 64:
        raise Rejected("tree_limit")
    frames = [app.get_child_at_index(i) for i in range(app.get_child_count())]
    active = []
    for frame in frames:
        if frame:
            frame.clear_cache()
            if frame.get_state_set().contains(api.StateType.ACTIVE):
                active.append(frame)
    if len(active) != 1:
        raise Rejected('ambiguous_active_window' if active else 'accessibility_unavailable')
    # Chromium implements Collection on the target frame. Query at most two
    # candidates server-side so a long conversation does not require visiting
    # every text node. Role validation remains local (no password role filter).
    collection = getattr(active[0], 'get_collection_iface', lambda: None)()
    if collection:
        try:
            rule = api.MatchRule.new(
                api.StateSet.new([api.StateType.FOCUSED, api.StateType.EDITABLE]),
                api.CollectionMatchType.ALL, {}, api.CollectionMatchType.ALL,
                [], api.CollectionMatchType.ALL, [], api.CollectionMatchType.ALL, False)
            matches = api.Collection.get_matches(collection, rule,
                api.CollectionSortOrder.CANONICAL, 2, True)
        except Exception:
            matches = None  # providers without Collection use the bounded walk
        if matches is not None:
            if len(matches) != 1:
                raise Rejected('ambiguous_focus' if matches else 'no_focused_editable')
            node = matches[0]
            node.clear_cache()
            state = node.get_state_set()
            role = node.get_role()
            if role == api.Role.PASSWORD_TEXT:
                raise Rejected('sensitive_field')
            if role not in (api.Role.ENTRY, api.Role.TEXT):
                raise Rejected('unsupported_editable_role')
            if not all(state.contains(s) for s in (api.StateType.FOCUSED,
                       api.StateType.EDITABLE, api.StateType.ENABLED, api.StateType.SHOWING)):
                raise Rejected('focus_changed')
            if time.monotonic() > deadline:
                raise Rejected('timeout')
            return node
    stack, found, visited = list(active), [], 0
    while stack:
        if time.monotonic() > deadline:
            raise Rejected('timeout')
        visited += 1
        if visited > MAX_NODES:
            raise Rejected('tree_limit')
        node = stack.pop()
        if not node:
            continue
        # AT-SPI caches state/children; a second traversal alone is not freshness.
        node.clear_cache()
        state = node.get_state_set()
        if not state.contains(api.StateType.SHOWING):
            continue
        role = node.get_role()
        if state.contains(api.StateType.FOCUSED):
            if role == api.Role.PASSWORD_TEXT:
                raise Rejected('sensitive_field')
            if state.contains(api.StateType.EDITABLE):
                if role not in (api.Role.ENTRY, api.Role.TEXT):
                    raise Rejected('unsupported_editable_role')
                if not state.contains(api.StateType.ENABLED):
                    raise Rejected('disabled_field')
                found.append(node)
        # Password descendants must never be visited/read.
        if role != api.Role.PASSWORD_TEXT:
            count = node.get_child_count()
            if count > MAX_NODES:
                raise Rejected('tree_limit')
            stack.extend(node.get_child_at_index(i) for i in range(count))
    if len(found) != 1:
        raise Rejected('ambiguous_focus' if found else 'no_focused_editable')
    return found[0]


def selected_range(field, api):
    iface = field.get_text_iface()
    if not iface:
        raise Rejected('missing_text_interface')
    count = api.Text.get_n_selections(iface)
    if count != 1:
        raise Rejected('no_selection' if count == 0 else 'ambiguous_selection')
    span = api.Text.get_selection(iface, 0)
    start, end = span.start_offset, span.end_offset
    if not 0 <= start < end <= api.Text.get_character_count(iface):
        raise Rejected('invalid_selection')
    if end - start > MAX_SELECTION:
        raise Rejected('selection_too_long')
    return iface, start, end


def request_accessibility(app, api):
    """One target-window request; Chromium uses these AT APIs to enable AX.

    Discard attributes/relations. Never toggle desktop accessibility settings or
    enumerate another application's content. Older providers may not respond.
    """
    app.clear_cache()
    frames = []
    for i in range(min(app.get_child_count(), 64)):
        frame = app.get_child_at_index(i)
        if frame:
            frame.clear_cache()
            if frame.get_state_set().contains(api.StateType.ACTIVE):
                frames.append(frame)
    if len(frames) != 1:
        raise Rejected('accessibility_unavailable')
    frames[0].get_attributes()
    frames[0].get_relation_set()


def identity(field):
    bus, path = field.app.bus_name, field.path
    if not bus or not path or len(bus) + len(path) > 512:
        raise Rejected('missing_field_identity')
    return bus + ':' + path


def capture(program, api, desktop, window_reader=active_window,
            matcher=program_matches, budget=.30):
    deadline = time.monotonic() + budget
    initial_window = window_reader()
    if not matcher(program, initial_window[1]):
        raise Rejected('program_pid_mismatch')
    apps = [desktop.get_child_at_index(i) for i in range(min(desktop.get_child_count(), 128))]
    apps = [app for app in apps if app and app.get_process_id() == initial_window[1]]
    if len(apps) != 1:
        raise Rejected('accessibility_unavailable')
    try:
        field = focused_field(apps[0], api, deadline)
    except Rejected as exc:
        if str(exc) != 'no_focused_editable':
            raise
        request_accessibility(apps[0], api)
        time.sleep(.05)
        field = focused_field(apps[0], api, deadline)
    field_id = identity(field)
    iface, start, end = selected_range(field, api)
    # This is the only text read in the helper: exactly the explicit selection.
    text = api.Text.get_text(iface, start, end)
    if not isinstance(text, str) or len(text) != end - start or '\x00' in text:
        raise Rejected('invalid_selection')
    if not text.strip():
        raise Rejected('empty_selection')
    again = focused_field(apps[0], api, deadline)
    _, again_start, again_end = selected_range(again, api)
    if identity(again) != field_id or (again_start, again_end) != (start, end):
        raise Rejected('selection_changed')
    if api.Text.get_text(again.get_text_iface(), start, end) != text:
        raise Rejected('selection_changed')
    if window_reader() != initial_window or not matcher(program, initial_window[1]):
        raise Rejected('focus_changed')
    if time.monotonic() > deadline:
        raise Rejected('timeout')
    return {'type': 'selection', 'program': program, 'text': text,
            'context_id': field_id, 'value': f'{initial_window[0]}:{initial_window[1]}',
            'selection_start': start, 'selection_end': end}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--program', required=True)
    args = parser.parse_args()
    def timeout(*_):
        raise Rejected('timeout')
    signal.signal(signal.SIGALRM, timeout)
    signal.setitimer(signal.ITIMER_REAL, .40)
    try:
        import gi
        gi.require_version('Atspi', '2.0')
        from gi.repository import Atspi
        Atspi.set_timeout(50, 80)
        result = capture(args.program, Atspi, Atspi.get_desktop(0))
    except Rejected as exc:
        result = {'type': 'selection_error', 'code': str(exc)}
    except (ImportError, ValueError):
        result = {'type': 'selection_error', 'code': 'missing_atspi_dependency'}
    except FileNotFoundError:
        result = {'type': 'selection_error', 'code': 'missing_xprop_dependency'}
    except subprocess.TimeoutExpired:
        result = {'type': 'selection_error', 'code': 'timeout'}
    except Exception:
        # Provider exceptions may contain application content: never print them.
        result = {'type': 'selection_error', 'code': 'accessibility_unavailable'}
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    print(json.dumps(result, ensure_ascii=False, separators=(',', ':')))


if __name__ == '__main__':
    main()
