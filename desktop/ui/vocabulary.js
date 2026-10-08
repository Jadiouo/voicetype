// No transcript polling: vocabulary/preview only responds to explicit actions.
const $ = selector => document.querySelector(selector);
let view;
let busy = false;
let index = null;
const lines = value => [...new Set(value.split(/\r?\n/).map(s => s.trim()).filter(Boolean))];
function controls() {
  $('#vocab-fields').disabled = busy || !view;
  for (const button of document.querySelectorAll('#vocabulary button')) button.disabled = busy || !view;
  $('#vocab-reload').disabled = busy;
  $('#vocab-save-names').disabled = busy || !view?.name_conversion_available;
  $('#vocab-names').disabled = busy || !view?.name_conversion_available;
  $('#vocab-terms').disabled = busy || !view;
}
function resetForm() {
  index = null;
  $('#vocab-form').reset();
  $('#vocab-edit-title').textContent = '新增修正规則';
}
function render(next) {
  view = next;
  const list = $('#vocab-list');
  list.replaceChildren();
  if (!next.entries.length) {
    const empty = document.createElement('p');
    empty.textContent = '還沒有修正规則。可以先填入下方 GitHub 範例，再按儲存。';
    list.append(empty);
  }
  next.entries.forEach((entry, row) => {
    const item = document.createElement('div');
    item.className = 'vocab-row';
    const text = document.createElement('span');
    text.textContent = `${entry.wrong.join('、')} → ${entry.right}`;
    const edit = document.createElement('button');
    edit.type = 'button'; edit.textContent = '編輯';
    edit.addEventListener('click', () => {
      index = row;
      $('#vocab-wrong').value = entry.wrong.join('\n');
      $('#vocab-right').value = entry.right;
      $('#vocab-edit-title').textContent = '編輯修正规則';
      $('#vocab-wrong').focus();
    });
    const remove = document.createElement('button');
    remove.type = 'button'; remove.textContent = '刪除';
    remove.addEventListener('click', () => change({ kind: 'delete', index: row }));
    item.append(text, edit, remove); list.append(item);
  });
  $('#vocab-names').value = next.names.join('\n');
  $('#vocab-terms').value = next.terms.join('\n');
  $('#vocab-names-note').textContent = next.name_conversion_available
    ? '名字依你的字形保留，例如台積電、游錫堃。'
    : '此安裝尚缺 OpenCC 繁體資料，暫時無法儲存保護名字；已存在的名字不會被忽略。';
}
async function run(action, message) {
  if (busy) return;
  busy = true; controls();
  $('#vocab-error').hidden = true;
  $('#vocab-status').textContent = '正在處理詞庫…';
  try {
    await action();
    $('#vocab-status').textContent = message;
  } catch (reason) {
    $('#vocab-error').textContent = String(reason);
    $('#vocab-error').hidden = false;
    $('#vocab-status').textContent = '操作未完成。編輯內容保留，可修正後重試或重新載入詞庫。';
  } finally { busy = false; controls(); }
}
const invoke = (command, args = {}) => window.__TAURI__.core.invoke(command, args);
function change(change) {
  if (!view) return;
  return run(async () => {
    render(await invoke('edit_vocabulary', { revision: view.revision, change }));
    resetForm();
    $('#vocab-preview-output').textContent = '';
  }, '詞庫已儲存，App 本機聽寫會在下一句套用。');
}
function reload() {
  return run(async () => { render(await invoke('get_vocabulary')); resetForm(); }, '詞庫已載入。');
}
$('#vocab-form').addEventListener('submit', event => {
  event.preventDefault();
  change({ kind: 'put', index, wrong: lines($('#vocab-wrong').value), right: $('#vocab-right').value.trim() });
});
$('#vocab-reload').addEventListener('click', reload);
$('#vocab-new').addEventListener('click', resetForm);
$('#vocab-example').addEventListener('click', () => {
  resetForm(); $('#vocab-wrong').value = 'geeho\ngit hub'; $('#vocab-right').value = 'GitHub';
});
$('#vocab-import').addEventListener('click', () => change({ kind: 'import' }));
$('#vocab-restore').addEventListener('click', () => change({ kind: 'restore' }));
$('#vocab-save-names').addEventListener('click', () => change({ kind: 'names', names: lines($('#vocab-names').value) }));
$('#vocab-save-terms').addEventListener('click', () => change({ kind: 'terms', terms: lines($('#vocab-terms').value) }));
$('#vocab-preview').addEventListener('click', () => run(async () => {
  $('#vocab-preview-output').textContent = await invoke('preview_vocabulary', {
    revision: view.revision, text: $('#vocab-preview-input').value,
  });
}, '已預覽儲存的詞庫替換；不包含語音辨識、繁體轉換或模型校正。'));
reload();
