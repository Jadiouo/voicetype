"""Human review pane. Audio is played only after an explicit button press."""
from datetime import datetime
import gi
gi.require_version('Gtk', '3.0')
gi.require_version('Gst', '1.0')
from gi.repository import Gtk, Gst

from voicetype_review import ReviewStore, propose_pair, read_config, set_enabled


def text(view):
    buffer = view.get_buffer()
    return buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), True)


def editor(height, editable=True):
    view = Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR, editable=editable)
    for side in ('left', 'right', 'top', 'bottom'):
        getattr(view, 'set_' + side + '_margin')(10)
    scroll = Gtk.ScrolledWindow(shadow_type=Gtk.ShadowType.IN)
    scroll.set_min_content_height(height)
    scroll.add(view)
    return view, scroll


def label(value):
    return Gtk.Label(label=value, xalign=0, wrap=True)


class ReviewPane(Gtk.Box):
    def __init__(self, window):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10, margin=16)
        self.window = window
        self.store = ReviewStore()
        self.current = None
        self.initial = ''
        self.loading = False
        self.player = None
        self.player_bus = None
        self.records = {}
        self.enabled = Gtk.CheckButton(label='抽樣待校對：每天最多 5 段')
        try:
            self.enabled.set_active(read_config()['enabled'])
        except (OSError, ValueError):
            self.enabled.set_active(False)
        self.enabled.connect('toggled', self.enable_changed)
        self.pack_start(self.enabled, False, False, 0)
        self.pack_start(label('8 秒以上錄音隨機抽約三分之一，只存本機。音訊與校對文字 7 天後清理；已加入的詞庫規則會保留。'), False, False, 0)
        pane = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL)
        pane.set_position(285)
        self.pack_start(pane, True, True, 0)
        left = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        self.model = Gtk.ListStore(str, str)
        self.tree = Gtk.TreeView(model=self.model, headers_visible=False)
        renderer = Gtk.CellRendererText(ellipsize=3, ypad=8)
        self.tree.append_column(Gtk.TreeViewColumn('樣本', renderer, text=1))
        self.tree.get_selection().connect('changed', self.selected)
        scroll = Gtk.ScrolledWindow(shadow_type=Gtk.ShadowType.IN)
        scroll.set_min_content_width(230)
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroll.add(self.tree)
        left.pack_start(scroll, True, True, 0)
        self.show_reviewed = Gtk.CheckButton(label='也顯示已校對')
        self.show_reviewed.connect('toggled', lambda _: self.refresh())
        left.pack_start(self.show_reviewed, False, False, 0)
        refresh = Gtk.Button(label='重新整理')
        refresh.connect('clicked', lambda _: self.refresh())
        left.pack_start(refresh, False, False, 0)
        pane.pack1(left, resize=False, shrink=False)
        right = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8, margin_start=18)
        pane.pack2(right, resize=True, shrink=False)
        playback = Gtk.Box(spacing=8)
        self.play_button = Gtk.Button(label='▶ 重聽這段')
        self.play_button.connect('clicked', self.play)
        playback.pack_start(self.play_button, False, False, 0)
        stop = Gtk.Button(label='■ 停止')
        stop.connect('clicked', lambda _: self.stop())
        playback.pack_start(stop, False, False, 0)
        self.detail = label('選一段來聽，或之後再回來。')
        playback.pack_end(self.detail, False, False, 0)
        right.pack_start(playback, False, False, 0)
        right.pack_start(label('當時的輸出'), False, False, 0)
        self.original, scroll = editor(72, editable=False)
        right.pack_start(scroll, True, True, 0)
        right.pack_start(label('有錯就直接修改下方文字'), False, False, 0)
        self.corrected, scroll = editor(95)
        self.corrected.get_buffer().connect('changed', self.changed)
        right.pack_start(scroll, True, True, 0)
        self.suggestion = label('正確的句子直接按「這句正確」。')
        right.pack_start(self.suggestion, False, False, 0)
        row = Gtk.Box(spacing=6)
        self.correct_button = Gtk.Button(label='這句正確')
        self.correct_button.connect('clicked', lambda _: self.save(unchanged=True))
        row.pack_start(self.correct_button, False, False, 0)
        self.save_button = Gtk.Button(label='記錄修正')
        self.save_button.connect('clicked', lambda _: self.save())
        row.pack_start(self.save_button, False, False, 0)
        self.learn_button = Gtk.Button(label='記錄並加入詞庫')
        self.learn_button.get_style_context().add_class('suggested-action')
        self.learn_button.connect('clicked', lambda _: self.save(learn=True))
        row.pack_end(self.learn_button, False, False, 0)
        right.pack_start(row, False, False, 0)
        self.delete_button = Gtk.Button(label='刪除這段音訊與文字')
        self.delete_button.connect('clicked', self.delete)
        right.pack_start(self.delete_button, False, False, 0)
        self.refresh()

    def dirty(self):
        return self.current is not None and text(self.corrected) != self.initial

    def enable_changed(self, widget):
        try:
            set_enabled(widget.get_active())
            self.window.tell('抽樣已開啟；之後符合長度的聽寫有機會留下。' if widget.get_active() else '已停止新增抽樣，既有資料仍可校對或刪除。')
        except (OSError, ValueError) as error:
            widget.handler_block_by_func(self.enable_changed)
            widget.set_active(not widget.get_active())
            widget.handler_unblock_by_func(self.enable_changed)
            self.window.error(error)

    def refresh(self):
        try:
            records = self.store.list()
        except (OSError, ValueError) as error:
            # The parent may still be under construction; retain message locally.
            self.detail.set_text(str(error))
            return
        self.records = {r['id']: r for r in records}
        self.loading = True
        self.model.clear()
        selected = False
        for record in records:
            if record['status'] != 'pending' and not self.show_reviewed.get_active():
                continue
            stamp = datetime.fromtimestamp(record['created_at']).strftime('%m/%d %H:%M')
            prefix = '✓ ' if record['status'] != 'pending' else ''
            title = f"{prefix}{stamp} · {record.get('duration_ms', 0)/1000:.0f} 秒\n{record['output_text'][:60]}"
            row = self.model.append((record['id'], title))
            if self.current and self.current['id'] == record['id']:
                self.tree.get_selection().select_iter(row)
                selected = True
        self.loading = False
        if not selected and not self.dirty():
            self.current = None
            if len(self.model):
                self.tree.get_selection().select_path(0)
            else:
                self.show_record(None)

    def selected(self, selection):
        if self.loading:
            return
        model, row = selection.get_selected()
        if row is None:
            return
        record = self.records[model[row][0]]
        if self.current and record['id'] == self.current['id']:
            return
        if self.dirty() and not self.window.confirm('這段校對尚未儲存，要放棄修改嗎？'):
            self.refresh()
            return
        self.show_record(record)

    def show_record(self, record):
        self.stop()
        self.current = record
        original = record['output_text'] if record else ''
        corrected = (record.get('pending_corrected_text') or record.get('corrected_text') or original) if record else ''
        self.original.get_buffer().set_text(original)
        self.corrected.get_buffer().set_text(corrected)
        self.initial = corrected
        self.detail.set_text(f"{record.get('duration_ms',0)/1000:.1f} 秒" if record else '目前沒有待校對。照常說話即可。')
        for control in (self.play_button, self.correct_button, self.save_button, self.delete_button):
            control.set_sensitive(record is not None)
        self.changed(None)

    def changed(self, _):
        if not hasattr(self, 'suggestion'):
            return
        corrected = text(self.corrected)
        pair = propose_pair(self.current['output_text'], corrected) if self.current else None
        self.learn_button.set_sensitive(pair is not None)
        self.correct_button.set_sensitive(self.current is not None and corrected == self.current['output_text'])
        self.suggestion.set_text(f'加入詞庫會記住：{pair[0]} → {pair[1]}' if pair else
            '大幅改寫先記錄整句；只有明確短修正才提供加入詞庫。')

    def save(self, unchanged=False, learn=False):
        if self.current is None:
            return
        corrected = self.current['output_text'] if unchanged else text(self.corrected)
        try:
            if learn and (self.window.form_dirty() or self.window.names_dirty()):
                raise ValueError('詞庫頁或名字保護還有未儲存的修改，請先儲存，再加入這筆詞對。')
            pair = self.store.review(self.current['id'], corrected,
                                     expected_output=self.current['output_text'], learn=learn)
            if pair:
                # Reload both the snapshot and form: an external edit may have
                # reordered entries, so retaining the old numeric index is unsafe.
                self.window.reload()
            self.stop()
            self.current = None
            self.initial = ''
            self.refresh()
            self.window.get_application().refresh_review_count()
            self.window.tell('已記錄並加入詞庫，下一句生效。' if pair else '已記錄校對結果。')
        except (OSError, ValueError) as error:
            self.window.error(error)

    def delete(self, _):
        if self.current is None or not self.window.confirm('刪除這段音訊與文字？已加入的詞庫規則會保留。'):
            return
        try:
            self.stop()
            self.store.delete(self.current['id'])
            self.current = None
            self.initial = ''
            self.refresh()
            self.window.get_application().refresh_review_count()
            self.window.tell('這段音訊與文字已刪除。')
        except (OSError, ValueError) as error:
            self.window.error(error)

    def play(self, _):
        if self.current is None:
            return
        try:
            self.stop()
            path = self.store.audio_path(self.current['id'])
            Gst.init(None)
            self.player = Gst.ElementFactory.make('playbin', None)
            if self.player is None:
                raise ValueError('找不到本機音訊播放器。')
            self.player.set_property('uri', path.resolve().as_uri())
            self.player.set_property('video-sink', Gst.ElementFactory.make('fakesink', None))
            self.player_bus = self.player.get_bus()
            self.player_bus.add_signal_watch()
            self.player_bus.connect('message', self.playback_message)
            result = self.player.set_state(Gst.State.PLAYING)
            if result == Gst.StateChangeReturn.FAILURE:
                raise ValueError('音訊無法播放。')
            self.play_button.set_sensitive(False)
        except (OSError, ValueError) as error:
            self.stop()
            self.window.error(error)

    def playback_message(self, _, message):
        if message.type == Gst.MessageType.ERROR:
            error, _ = message.parse_error()
            self.stop()
            self.window.error('重聽失敗：' + error.message)
        elif message.type == Gst.MessageType.EOS:
            self.stop()

    def stop(self):
        if self.player is not None:
            self.player.set_state(Gst.State.NULL)
            if self.player_bus:
                self.player_bus.remove_signal_watch()
            self.player, self.player_bus = None, None
        if hasattr(self, 'play_button'):
            self.play_button.set_sensitive(self.current is not None)
