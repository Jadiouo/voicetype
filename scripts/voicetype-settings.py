#!/usr/bin/env python3
"""GTK vocabulary editor and tray entry, separate from speech processing."""
import argparse
import json
import os
from pathlib import Path
import socket
import sys
import threading

import gi
gi.require_version("Gtk", "3.0")
gi.require_version("AyatanaAppIndicator3", "0.1")
from gi.repository import AyatanaAppIndicator3 as Indicator, Gdk, Gio, GLib, Gtk

from voicetype_vocab import Vocabulary, atomic_write
from voicetype_review import ReviewStore
from voicetype_review_gui import ReviewPane

APP_ID = "io.github.voicetype.Settings"
EXAMPLES = [
    ("GitHub：同一個詞的幾種錯法", ["gthub", "git hub"], "GitHub"),
    ("英文拼字：pull request", ["pull requset", "pull reqeust"], "pull request"),
    ("完整片語：避免改到正常 coming", ["把修改 coming"], "把修改 commit"),
]


def buffer_text(view):
    buffer = view.get_buffer()
    return buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), True)


def text_view(height=110, editable=True):
    view = Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR)
    view.set_editable(editable)
    view.set_top_margin(10)
    view.set_bottom_margin(10)
    view.set_left_margin(10)
    view.set_right_margin(10)
    scroll = Gtk.ScrolledWindow()
    scroll.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
    scroll.set_shadow_type(Gtk.ShadowType.IN)
    scroll.set_min_content_height(height)
    scroll.add(view)
    return view, scroll


def label(text, style=None):
    widget = Gtk.Label(label=text, xalign=0)
    widget.set_line_wrap(True)
    if style:
        widget.get_style_context().add_class(style)
    return widget


def button(text, callback, style=None):
    widget = Gtk.Button(label=text)
    widget.connect("clicked", callback)
    if style:
        widget.get_style_context().add_class(style)
    return widget


def autostart_path():
    return Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "autostart" / (APP_ID + ".desktop")


def desktop_entry(launcher, tray=False):
    # Desktop Exec uses its own quoting, not shell quoting.
    path = str(launcher)
    if any(c in path for c in "\n\r\0"):
        raise ValueError("啟動路徑含不支援的字元。")
    for character in ('\\', '"', '`', '$'):
        path = path.replace(character, '\\' + character)
    path = path.replace('%', '%%')
    command = '"' + path + '"' + (" --tray" if tray else "")
    return ("[Desktop Entry]\nType=Application\nName=VoiceType 詞庫\n"
            "Comment=編輯語音輸入的個人詞庫\nExec=" + command + "\n"
            "Icon=voicetype-settings\nTerminal=false\nCategories=Settings;\n"
            "StartupWMClass=voicetype-settings\nX-GNOME-Autostart-enabled=true\n")


def preview_request(text):
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    default = Path(runtime) / "voicetype/ipc.sock" if runtime else Path(f"/tmp/voicetype-{os.getuid()}/ipc.sock")
    path = os.environ.get("VOICETYPE_SOCKET", str(default))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(3)
        connection.connect(path)
        connection.sendall(json.dumps({"type": "process_text", "text": text}, ensure_ascii=False).encode() + b"\n")
        data = bytearray()
        while b"\n" not in data:
            chunk = connection.recv(4096)
            if not chunk:
                raise ValueError("語音服務未回傳結果。")
            data.extend(chunk)
            if len(data) > 1024 * 1024:
                raise ValueError("語音服務回傳內容過長。")
    result = json.loads(data.split(b"\n", 1)[0])
    if result.get("type") == "error":
        raise ValueError(result.get("text", "語音服務發生錯誤。"))
    value = result.get("value", {})
    if not isinstance(value, dict) or not isinstance(value.get("text"), str):
        raise ValueError("語音服務回傳格式不正確。")
    return value["text"]


class SettingsWindow(Gtk.ApplicationWindow):
    def __init__(self, application):
        super().__init__(application=application, title="VoiceType 詞庫")
        self.set_default_size(960, 690)
        self.set_position(Gtk.WindowPosition.CENTER)
        self.set_icon_name("voicetype-settings")
        self.connect("delete-event", self.hide_window)
        self.vocabulary = None
        self.index = None
        self.form_initial = ("", "")
        self.loading = False
        self.preview_running = False
        header = Gtk.HeaderBar(title="VoiceType 詞庫", subtitle="儲存後，下一句自動套用")
        header.set_show_close_button(True)
        self.set_titlebar(header)
        header.pack_end(button("重新載入", self.reload_clicked))
        root = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin=18)
        self.add(root)
        notebook = self.notebook = Gtk.Notebook()
        root.pack_start(notebook, True, True, 0)
        notebook.append_page(self.build_vocabulary(), Gtk.Label(label="詞彙修正"))
        notebook.append_page(self.build_names(), Gtk.Label(label="名字保護"))
        self.review_pane = ReviewPane(self)
        self.review_tab = Gtk.Label(label="待校對")
        notebook.append_page(self.review_pane, self.review_tab)
        notebook.append_page(self.build_preview(), Gtk.Label(label="試打看看"))
        notebook.append_page(self.build_settings(), Gtk.Label(label="設定與說明"))
        self.status = label("", "dim-label")
        root.pack_end(self.status, False, False, 0)
        self.reload()

    def build_vocabulary(self):
        pane = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL, margin=16)
        pane.set_position(310)
        left = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        left.set_size_request(250, -1)
        self.search = Gtk.SearchEntry(placeholder_text="搜尋正確詞或辨識錯字")
        self.search.connect("search-changed", lambda _: self.populate())
        left.pack_start(self.search, False, False, 0)
        self.store = Gtk.ListStore(int, str, str)
        self.tree = Gtk.TreeView(model=self.store, headers_visible=False)
        renderer = Gtk.CellRendererText()
        renderer.set_property("ellipsize", 3)
        renderer.set_property("ypad", 9)
        self.tree.append_column(Gtk.TreeViewColumn("詞彙", renderer, text=1))
        self.tree.get_selection().connect("changed", self.selection_changed)
        scroll = Gtk.ScrolledWindow()
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroll.set_shadow_type(Gtk.ShadowType.IN)
        scroll.add(self.tree)
        left.pack_start(scroll, True, True, 0)
        left.pack_start(button("＋ 新增詞彙", self.new_clicked), False, False, 0)
        pane.pack1(left, resize=False, shrink=False)
        right = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10, margin_start=22)
        right.pack_start(label("應該改成什麼", "title"), False, False, 0)
        self.correct = Gtk.Entry(placeholder_text="例如：GitHub", max_length=64)
        self.correct.connect("changed", self.form_changed)
        right.pack_start(self.correct, False, False, 0)
        right.pack_start(label("辨識成什麼 · 每行一種錯法", "title"), False, False, 0)
        self.wrongs, scroll = text_view()
        self.wrongs.get_buffer().connect("changed", self.form_changed)
        right.pack_start(scroll, True, True, 0)
        self.example_label = label("例如：gthub → GitHub", "dim-label")
        right.pack_start(self.example_label, False, False, 0)
        right.pack_start(label("只替換列出的詞，不猜相似字。coming 等正常英文，請用完整片語。", "dim-label"), False, False, 0)
        examples = Gtk.ComboBoxText()
        examples.append_text("填入範例，再照著改…")
        for name, _, _ in EXAMPLES:
            examples.append_text(name)
        examples.set_active(0)
        examples.connect("changed", self.example_chosen)
        right.pack_start(examples, False, False, 0)
        actions = Gtk.Box(spacing=8)
        self.delete_button = button("刪除這筆", self.delete_clicked)
        self.save_button = button("儲存這筆", self.save_clicked, "suggested-action")
        actions.pack_start(self.delete_button, False, False, 0)
        actions.pack_end(self.save_button, False, False, 0)
        right.pack_start(actions, False, False, 0)
        pane.pack2(right, resize=True, shrink=False)
        return pane

    def build_names(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin=20)
        box.pack_start(label("保留專有名詞的正確寫法", "title"), False, False, 0)
        box.pack_start(label("每行一個名字，例如 GitHub 或人名。保護已出現的完整名字，避免被繁體轉換或校字改掉；辨識錯字請到「詞彙修正」新增。"), False, False, 0)
        self.names_view, scroll = text_view(210)
        box.pack_start(scroll, True, True, 0)
        box.pack_start(button("儲存名字", self.save_names, "suggested-action"), False, False, 0)
        return box

    def build_preview(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin=20)
        box.pack_start(label("測試已儲存的詞庫", "title"), False, False, 0)
        box.pack_start(label("先儲存規則，再貼上句子。這裡會經過目前的文字修正流程，只顯示結果，不錄音、不送字。", "dim-label"), False, False, 0)
        self.preview_input, scroll = text_view(100)
        self.preview_input.get_buffer().set_text("把修改 coming，再 push 到 git hub。")
        box.pack_start(scroll, True, True, 0)
        self.preview_button = button("看看修正結果", self.preview_clicked, "suggested-action")
        box.pack_start(self.preview_button, False, False, 0)
        box.pack_start(label("修正結果", "title"), False, False, 0)
        self.preview_output, scroll = text_view(100, editable=False)
        box.pack_start(scroll, True, True, 0)
        return box

    def build_settings(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16, margin=20)
        box.pack_start(label("使用方式", "title"), False, False, 0)
        box.pack_start(label("1. 選左邊現有詞，或按「新增詞彙」。\n2. 填正確寫法，再填每種辨識錯字。\n3. 按「儲存這筆」，下一句自動生效。\n\n按住 Ctrl＋Alt 說話，放開送字。關閉這個視窗後，右上角圖示仍保留。"), False, False, 0)
        self.autostart = Gtk.CheckButton(label="登入 Linux 時顯示 VoiceType 圖示")
        self.autostart.set_active(autostart_path().exists())
        self.autostart.connect("toggled", self.autostart_changed)
        box.pack_start(self.autostart, False, False, 0)
        box.pack_start(button("還原上次儲存的詞庫", self.restore_clicked), False, False, 0)
        box.pack_start(label("詞庫每次儲存都保留上一版，供誤改時還原。此處編輯的是個人詞庫；既有修正學習仍由語音服務管理。", "dim-label"), False, False, 0)
        self.path_label = label("", "dim-label")
        self.path_label.set_selectable(True)
        box.pack_start(self.path_label, False, False, 0)
        return box

    def tell(self, message):
        self.status.set_text(message)

    def error(self, error):
        self.tell("未完成：" + str(error))

    def confirm(self, text):
        dialog = Gtk.MessageDialog(transient_for=self, modal=True,
                                   message_type=Gtk.MessageType.QUESTION,
                                   buttons=Gtk.ButtonsType.NONE, text=text)
        dialog.add_button("取消", Gtk.ResponseType.CANCEL)
        dialog.add_button("繼續", Gtk.ResponseType.OK)
        result = dialog.run()
        dialog.destroy()
        return result == Gtk.ResponseType.OK

    def form_value(self):
        return self.correct.get_text(), buffer_text(self.wrongs)

    def form_dirty(self):
        return self.form_value() != self.form_initial

    def names_dirty(self):
        return self.vocabulary is not None and buffer_text(self.names_view) != "\n".join(self.vocabulary.names)

    def discard_form(self):
        return not self.form_dirty() or self.confirm("這筆還沒儲存，要放棄修改嗎？")

    def reload_clicked(self, _):
        if (not self.form_dirty() and not self.names_dirty()) or self.confirm("重新載入會放棄尚未儲存的編輯，繼續嗎？"):
            self.reload()

    def reload(self):
        try:
            vocabulary = Vocabulary()
        except (OSError, ValueError) as error:
            self.vocabulary = None
            self.populate()
            self.names_view.get_buffer().set_text("")
            self.set_form(None)
            self.error(error)
            return
        self.vocabulary = vocabulary
        self.names_view.get_buffer().set_text("\n".join(vocabulary.names))
        self.path_label.set_text("詞庫位置：" + str(vocabulary.path))
        selected = next((i for i, entry in enumerate(vocabulary.entries) if entry["right"] == "GitHub"),
                        0 if vocabulary.entries else None)
        self.set_form(selected)
        self.populate()
        self.tell(f"已載入 {len(vocabulary.entries)} 筆詞彙。儲存後下一句生效。")

    def populate(self):
        self.loading = True
        self.store.clear()
        if self.vocabulary:
            query = self.search.get_text().lower()
            for i, entry in enumerate(self.vocabulary.entries):
                wrong = "、".join(entry["wrong"])
                if query in (entry["right"] + " " + wrong).lower():
                    row = self.store.append((i, entry["right"] + "\n" + wrong, wrong))
                    if self.index == i:
                        self.tree.get_selection().select_iter(row)
        self.loading = False

    def set_form(self, index, wrongs=None, right=""):
        self.index = index
        if index is not None:
            entry = self.vocabulary.entries[index]
            wrongs, right = entry["wrong"], entry["right"]
        self.correct.set_text(right)
        self.wrongs.get_buffer().set_text("\n".join(wrongs or []))
        self.form_initial = self.form_value()
        self.delete_button.set_sensitive(index is not None and self.vocabulary is not None)
        self.save_button.set_sensitive(self.vocabulary is not None)

    def selection_changed(self, selection):
        if self.loading:
            return
        model, row = selection.get_selected()
        if row is None:
            return
        index = model[row][0]
        if index == self.index:
            return
        if self.discard_form():
            self.set_form(index)
        else:
            self.populate()

    def new_clicked(self, _):
        if self.discard_form():
            self.set_form(None)
            self.tree.get_selection().unselect_all()
            self.correct.grab_focus()

    def form_changed(self, _):
        wrongs = buffer_text(self.wrongs).splitlines()
        right = self.correct.get_text()
        self.example_label.set_text((wrongs[0] + "  →  " + right) if wrongs and right else "例如：gthub → GitHub")

    def example_chosen(self, combo):
        selected = combo.get_active() - 1
        if selected < 0:
            return
        if self.discard_form():
            _, wrongs, right = EXAMPLES[selected]
            self.set_form(None)
            self.correct.set_text(right)
            self.wrongs.get_buffer().set_text("\n".join(wrongs))
            self.tell("範例已填入，確認內容後按「儲存這筆」。")
        combo.set_active(0)

    def save_clicked(self, _):
        if self.vocabulary is None:
            return
        wrongs = [line.strip() for line in buffer_text(self.wrongs).splitlines() if line.strip()]
        try:
            index = self.index if self.index is not None else len(self.vocabulary.entries)
            self.vocabulary.put(self.index, wrongs, self.correct.get_text().strip())
            self.set_form(index)
            self.populate()
            self.tell("已儲存，下一句自動套用。可到「試打看看」檢查。")
        except (OSError, ValueError) as error:
            self.error(error)

    def delete_clicked(self, _):
        if self.index is None or not self.confirm("刪除這筆詞彙？可在設定中還原上次儲存。"):
            return
        try:
            self.vocabulary.delete(self.index)
            self.set_form(None)
            self.populate()
            self.tell("已刪除這筆詞彙。")
        except (OSError, ValueError) as error:
            self.error(error)

    def save_names(self, _):
        if self.vocabulary is None:
            return
        try:
            self.vocabulary.set_names([line.strip() for line in buffer_text(self.names_view).splitlines() if line.strip()])
            self.names_view.get_buffer().set_text("\n".join(self.vocabulary.names))
            self.tell("名字已儲存，下一句自動套用。")
        except (OSError, ValueError) as error:
            self.error(error)

    def restore_clicked(self, _):
        if self.vocabulary is None or not self.confirm("將整份詞庫還原到上次儲存之前，並放棄未儲存編輯，繼續嗎？"):
            return
        try:
            self.vocabulary.restore()
            self.reload()
            self.tell("已還原上次儲存的詞庫。")
        except (OSError, ValueError) as error:
            self.error(error)

    def autostart_changed(self, widget):
        try:
            path = autostart_path()
            if widget.get_active():
                launcher = os.environ.get("VOICETYPE_SETTINGS_LAUNCHER", str(Path.home() / ".local/bin/voicetype-settings"))
                if not Path(launcher).is_file():
                    raise ValueError("請先安裝 VoiceType 詞庫設定，才能設定登入啟動。")
                atomic_write(path, desktop_entry(launcher, tray=True).encode())
            else:
                path.unlink(missing_ok=True)
            self.tell("登入啟動設定已更新。")
        except (OSError, ValueError) as error:
            widget.handler_block_by_func(self.autostart_changed)
            widget.set_active(path.exists())
            widget.handler_unblock_by_func(self.autostart_changed)
            self.error(error)

    def preview_clicked(self, _):
        text = buffer_text(self.preview_input)
        if not text.strip() or len(text) > 4096:
            self.tell("請輸入 1–4096 字的測試句。")
            return
        if self.form_dirty() or self.names_dirty():
            self.tell("還有未儲存編輯；請先儲存，再測試新規則。")
            return
        if self.preview_running:
            return
        self.preview_running = True
        self.preview_button.set_sensitive(False)
        self.preview_output.get_buffer().set_text("處理中…")

        def work():
            try:
                result, failed = preview_request(text), False
            except (OSError, ValueError) as error:
                result, failed = "無法預覽：" + str(error) + "\n詞庫仍可編輯；請確認語音服務已啟動。", True
            GLib.idle_add(self.preview_done, result, failed)
        threading.Thread(target=work, daemon=True).start()

    def preview_done(self, result, failed):
        self.preview_output.get_buffer().set_text(result)
        self.preview_running = False
        self.preview_button.set_sensitive(True)
        self.tell("預覽未完成。" if failed else "預覽完成，沒有送出文字或新增學習紀錄。")
        return GLib.SOURCE_REMOVE

    def hide_window(self, *_):
        self.review_pane.stop()
        self.hide()
        return True


class SettingsApplication(Gtk.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID, flags=Gio.ApplicationFlags.HANDLES_COMMAND_LINE)
        self.window = None

    def do_startup(self):
        Gtk.Application.do_startup(self)
        self.hold()
        GLib.set_application_name("VoiceType 詞庫")
        Gtk.Window.set_default_icon_name("voicetype-settings")
        self.indicator = Indicator.Indicator.new("voicetype-settings", "voicetype-settings", Indicator.IndicatorCategory.APPLICATION_STATUS)
        self.indicator.set_title("VoiceType 詞庫")
        self.indicator.set_status(Indicator.IndicatorStatus.ACTIVE)
        menu = Gtk.Menu()
        item = Gtk.MenuItem(label="VoiceType 詞庫與設定…")
        item.connect("activate", lambda _: self.activate())
        menu.append(item)
        self.review_menu = Gtk.MenuItem(label="待校對")
        self.review_menu.connect("activate", self.open_review)
        menu.append(self.review_menu)
        menu.append(Gtk.SeparatorMenuItem())
        item = Gtk.MenuItem(label="退出圖示（聽寫繼續運作）")
        item.connect("activate", self.quit_clicked)
        menu.append(item)
        menu.show_all()
        self.indicator.set_menu(menu)
        self.review_store = ReviewStore()
        self.refresh_review_count()
        GLib.timeout_add_seconds(60, self.refresh_review_count)

    def refresh_review_count(self):
        try:
            count = sum(r['status'] == 'pending' for r in self.review_store.list())
            self.indicator.set_label(str(count) if count else '', '35')
            self.review_menu.set_label(f"待校對 · {count} 段")
            if self.window:
                self.window.review_tab.set_text(f"待校對 ({count})" if count else '待校對')
        except (OSError, ValueError):
            pass  # A busy store is retried; it must not open a popup or steal focus.
        return GLib.SOURCE_CONTINUE

    def open_review(self, _):
        self.activate()
        self.window.review_pane.refresh()
        self.window.notebook.set_current_page(2)

    def do_activate(self):
        if self.window is None:
            self.window = SettingsWindow(self)
        self.window.show_all()
        self.window.present()

    def do_command_line(self, command_line):
        args = command_line.get_arguments()[1:]
        if args not in ([], ["--tray"]):
            command_line.printerr_literal("Usage: voicetype-settings [--tray]\n")
            return 2
        if "--tray" not in args:
            self.activate()
        return 0

    def quit_clicked(self, _):
        if self.window and (self.window.form_dirty() or self.window.names_dirty() or self.window.review_pane.dirty()):
            self.activate()
            if not self.window.confirm("有尚未儲存的修改，要放棄並退出圖示嗎？"):
                return
        if self.window:
            self.window.review_pane.stop()
        self.quit()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="VoiceType 詞庫設定與右上角圖示")
    parser.add_argument("--tray", action="store_true", help="只顯示圖示，不開啟視窗")
    parser.parse_args()
    raise SystemExit(SettingsApplication().run(sys.argv))
