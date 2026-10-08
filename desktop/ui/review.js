const $ = selector => document.querySelector(selector);
const invoke = (command, args = {}) => window.__TAURI__.core.invoke(command, args);
let view, selected, busy = false, dirty = false, audioURL;
const label = item => `${new Date(item.created_at * 1000).toLocaleString()} · ${(item.duration_ms / 1000).toFixed(1)} 秒 · ${item.promotion_pending ? '詞庫更新待重試' : ({pending: '待校對', correct: '已確認', corrected: '已修正'})[item.status]}`;
function releaseAudio() {
  const audio = $('#review-audio');
  audio.pause(); audio.removeAttribute('src'); audio.load(); audio.hidden = true;
  if (audioURL) URL.revokeObjectURL(audioURL);
  audioURL = undefined;
}
function controls() {
  for (const button of document.querySelectorAll('#review button')) button.disabled = busy;
  $('#review-enabled').disabled = busy || !view;
  $('#review-enabled').checked = view?.settings.enabled ?? false;
  $('#review-corrected').disabled = busy || selected?.promotion_pending;
  $('#review-save').disabled = busy || selected?.promotion_pending;
  $('#review-promote').disabled = busy || dirty;
}
function renderList() {
  const items = view.items;
  const pending = items.filter(i => i.status === 'pending' || i.promotion_pending).length;
  $('#review-count').textContent = pending ? `(${pending})` : '';
  $('#review-list').replaceChildren();
  if (!items.length) {
    const empty = document.createElement('p'); empty.textContent = '目前沒有待校對的錄音。';
    $('#review-list').append(empty);
  }
  for (const item of items) {
    const button = document.createElement('button'); button.type = 'button';
    button.textContent = label(item); button.dataset.id = item.id;
    button.setAttribute('aria-pressed', String(selected?.id === item.id));
    button.addEventListener('click', () => {
      if (dirty) { $('#review-status').textContent = '請先儲存這句修改；也可按重新載入，放棄尚未儲存的內容。'; return; }
      select(item); renderList(); controls();
    });
    $('#review-list').append(button);
  }
}
function select(item) {
  releaseAudio(); selected = item; dirty = false;
  $('#review-detail').hidden = !item;
  if (!item) return;
  $('#review-selected').textContent = label(item);
  $('#review-asr').textContent = item.asr_text;
  $('#review-output').textContent = item.output_text;
  $('#review-corrected').value = item.corrected_text ?? item.output_text;
  const pair = item.suggested_rule;
  $('#review-rule').textContent = pair
    ? `可加入詞庫：${pair.wrong} → ${pair.right}。這會套用到之後的聽寫。`
    : '只把單一短詞或片語修正加入詞庫；多處修改可到「我的詞庫」分別新增。';
  $('#review-promote').hidden = !pair;
  $('#review-promote').textContent = item.promotion_pending ? '重試加入詞庫' : '加入詞庫';
}
async function run(action, message) {
  if (busy) return;
  busy = true; controls(); $('#review-error').hidden = true;
  $('#review-status').textContent = '正在處理校對…';
  try { await action(); $('#review-status').textContent = message; }
  catch (reason) {
    $('#review-error').textContent = String(reason); $('#review-error').hidden = false;
    $('#review-status').textContent = '操作未完成，編輯內容保留。資料已變更時，請重新載入後再操作。';
  } finally { busy = false; controls(); }
}
async function refresh() {
  const next = await invoke('get_review');
  view = next;
  $('#review-policy').textContent = `每天最多 ${next.settings.daily_limit} 段，從 8 秒以上的聽寫抽樣。預設關閉，不會額外開啟麥克風。`;
  const current = next.items.find(i => i.id === selected?.id);
  select(current); renderList();
}
function reload() { return run(refresh, '校對資料已載入。'); }
async function edit(change, message) {
  if (!selected) return;
  const item = selected;
  return run(async () => {
    const saved = await invoke('edit_review', { id: item.id, revision: item.revision, change });
    view.items = view.items.filter(i => i.id !== item.id);
    if (saved) view.items.push(saved);
    view.items.sort((a, b) => b.created_at - a.created_at);
    select(saved); renderList();
    if (change.kind === 'promote') window.dispatchEvent(new Event('vocabulary-changed'));
  }, message);
}
$('#review-reload').addEventListener('click', reload);
$('#review-enabled').addEventListener('change', event => {
  const enabled = event.target.checked;
  run(async () => {
    view.settings = await invoke('set_review_enabled', { revision: view.settings.revision, enabled });
  }, enabled ? '抽樣已開啟，下次本機聽寫開始套用。' : '抽樣已關閉。既有樣本仍會在 7 天後清理。');
});
$('#review-corrected').addEventListener('input', () => { dirty = true; controls(); });
$('#review-form').addEventListener('submit', event => {
  event.preventDefault(); edit({ kind: 'save', corrected: $('#review-corrected').value }, '校對結果已儲存。');
});
$('#review-delete').addEventListener('click', () => edit({ kind: 'delete' }, '這段錄音與文字已刪除。'));
$('#review-promote').addEventListener('click', () => edit({ kind: 'promote' }, '規則已加入詞庫，從下一句本機聽寫開始套用。'));
$('#review-audio-load').addEventListener('click', () => run(async () => {
  releaseAudio();
  const bytes = await invoke('get_review_audio', { id: selected.id, revision: selected.revision });
  audioURL = URL.createObjectURL(new Blob([bytes instanceof ArrayBuffer ? bytes : new Uint8Array(bytes)], {type: 'audio/wav'}));
  $('#review-audio').src = audioURL; $('#review-audio').hidden = false;
}, '音訊已載入，按播放即可聆聽。'));
window.addEventListener('beforeunload', releaseAudio);
// Do not let an already-loaded audio blob outlive the seven-day retention time.
setInterval(() => {
  if (selected && Date.now() >= (selected.created_at + 7 * 86400) * 1000) {
    releaseAudio(); selected = undefined; $('#review-detail').hidden = true;
    $('#review-status').textContent = '這段校對資料已到期，請重新載入。';
  }
}, 1000);
window.addEventListener('review-open', () => {
  $('#review').scrollIntoView(); if (!dirty) reload();
});
reload();
