#!/usr/bin/env python3
"""Read bounded text from the active X11 application's accessibility tree.

Invoked once at push-to-talk start, not a background screen recorder. Returns
JSON; no screenshots, clipboard reads, network calls, or persistent transcript.
The daemon imposes its own hard timeout because accessibility providers can hang.
"""
import json
import re
import subprocess
import time


def prop(*args):
    return subprocess.run(["xprop", *args], capture_output=True, text=True,
                          timeout=0.15, check=False).stdout


def capture():
    active = re.search(r"0x[0-9a-fA-F]+", prop("-root", "_NET_ACTIVE_WINDOW"))
    if not active or active[0] == "0x0":
        return {"text": "", "source": "unavailable"}
    window = active[0]
    properties = prop("-id", window, "_NET_WM_PID", "_NET_WM_NAME", "WM_NAME")
    pid = re.search(r"_NET_WM_PID\([^)]*\) = (\d+)", properties)
    title = re.search(r'(?:_NET_WM_NAME|WM_NAME)\([^)]*\) = "(.*)"', properties)
    # The title is useful context even when the application has no AT-SPI text.
    parts = [title[1][:256]] if title else []
    source = "window-title" if parts else "unavailable"
    if pid:
        try:
            import gi
            gi.require_version("Atspi", "2.0")
            from gi.repository import Atspi
            Atspi.set_timeout(80, 120)
            deadline = time.monotonic() + 0.18
            desktop = Atspi.get_desktop(0)
            apps = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            app = next((a for a in apps if a and a.get_process_id() == int(pid[1])), None)
            if app:
                frames = [app.get_child_at_index(i) for i in range(app.get_child_count())]
                frames = [f for f in frames if f and f.get_state_set().contains(Atspi.StateType.ACTIVE)]
                stack, seen, visited = list(frames), set(parts), 0
                while stack and visited < 180 and time.monotonic() < deadline:
                    node = stack.pop()
                    visited += 1
                    if not node or node.get_role() == Atspi.Role.PASSWORD_TEXT:
                        continue
                    state = node.get_state_set()
                    if not state.contains(Atspi.StateType.SHOWING):
                        continue
                    # Exclude editable fields; Fcitx supplies the selected/current
                    # input separately. The tree is for visible page/reply text.
                    if not state.contains(Atspi.StateType.EDITABLE):
                        iface = node.get_text_iface()
                        if iface:
                            text = Atspi.Text.get_text(iface, 0, min(Atspi.Text.get_character_count(iface), 1024)).strip()
                            if text and text not in seen:
                                parts.append(text)
                                seen.add(text)
                                source = "accessibility"
                    if sum(map(len, parts)) >= 3072:
                        break
                    children = min(node.get_child_count(), 80)
                    stack.extend(node.get_child_at_index(i) for i in reversed(range(children)))
        except (ImportError, ValueError, RuntimeError, AttributeError, TypeError):
            pass
    return {"text": "\n".join(parts)[:3072], "source": source, "window_id": window}


if __name__ == "__main__":
    try:
        result = capture()
    except Exception:
        result = {"text": "", "source": "unavailable"}
    print(json.dumps(result, ensure_ascii=False))
