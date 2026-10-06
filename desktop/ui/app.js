const fieldset = document.querySelector('#providers');
const error = document.querySelector('#error');
const status = document.querySelector('#status');
const reload = document.querySelector('#reload');
const check = document.querySelector('#check-providers');
const cancel = document.querySelector('#cancel-dictation');
const choices = [...document.querySelectorAll('input[name="provider"]')];
let lastView;
let commandBusy = false;
let pollBusy = false;
let recoveryBusy = false;
let revision = 0;
let recovery;
const recoveryNote = document.querySelector('#recovery-note');
const recoveryContent = document.querySelector('#recovery-content');
const recoveryText = document.querySelector('#recovery-text');
const dismissRecovery = document.querySelector('#dismiss-recovery');

async function refreshRecovery() {
  const requestedRevision = revision;
  const next = await window.__TAURI__.core.invoke('get_recovery');
  if (requestedRevision !== revision) return;
  if (next?.session !== recovery?.session || next?.provider !== recovery?.provider) {
    recoveryText.value = next?.text || '';
  }
  recovery = next;
  recoveryContent.hidden = !next;
  recoveryNote.textContent = !next ? '目前沒有待處理的文字。'
    : next.reason === 'unconfirmed' ? '無法確認是否已送出，文字可能已在原欄位中。請先檢查原欄位，避免重複貼上。'
    : next.reason === 'partial' ? '可能只有部分文字送出。請先檢查原欄位，再選取需要的內容。'
    : '輸入位置已變更，完整文字保留在這裡。';
}

function render(view) {
  lastView = view;
  cancel.hidden = !view.settings.dictation.busy;
  cancel.disabled = view.settings.dictation.phase === 'releasing';
  for (const choice of choices) {
    choice.checked = choice.value === view.settings.selected_provider;
  }
  const labels = {
    not_connected: '準備中 · 尚未連接辨識引擎',
    service_available: '本機服務有回應 · App 錄音仍在整合中',
    offline: '找不到本機服務 · 請確認引擎已啟動',
    timed_out: '本機服務回應逾時 · 可稍後重新檢查',
    incompatible: '本機服務回應不相容 · 請檢查安裝版本',
  };
  for (const provider of view.settings.providers) {
    const label = document.querySelector(`#${provider.provider}-availability`);
    if (label) label.textContent = labels[provider.availability] || '尚未檢查服務';
  }
  if (view.local_runtime !== 'inactive') {
    const runtimeLabels = {
      waiting_for_input: '引擎已載入 · 等待輸入法連接',
      ready: '本機引擎與輸入法已連接',
      failed: '本機引擎已中斷 · 請重新準備',
    };
    document.querySelector('#local-availability').textContent = runtimeLabels[view.local_runtime] || '尚未準備引擎';
  }
  document.querySelector('#version').textContent = `開發預覽 ${view.settings.version}`;
  document.querySelector('#tray-note').textContent = view.tray_available
    ? '可從系統圖示開啟此視窗。關閉視窗會結束預覽 App。'
    : '此桌面未提供常駐圖示。關閉視窗會結束預覽 App。';
}

async function command(name, args = {}) {
  if (commandBusy) return;
  commandBusy = true;
  revision++;
  fieldset.disabled = true;
  reload.disabled = true;
  check.disabled = true;
  cancel.disabled = true;
  error.hidden = true;
  status.textContent = name === 'select_provider' ? '正在儲存…'
    : name === 'cancel_dictation' ? '正在取消聽寫…'
    : name === 'refresh_providers' ? '正在檢查服務…' : '正在讀取設定…';
  try {
    if (!window.__TAURI__) throw new Error('請透過已安裝的 VoiceType App 開啟設定。');
    const view = await window.__TAURI__.core.invoke(name, args);
    render(view);
    await refreshRecovery().catch(reason => { recoveryNote.textContent = String(reason); });
    status.textContent = name === 'select_provider'
      ? '偏好已儲存。現有聽寫方式尚未變更。'
      : name === 'cancel_dictation' ? '已要求取消，正在等待引擎停止。'
      : name === 'refresh_providers' ? '已檢查服務。此操作不會啟動錄音。'
      : '已讀取你的偏好設定。';
  } catch (reason) {
    if (lastView) render(lastView);
    error.textContent = String(reason);
    error.hidden = false;
    status.textContent = '尚未套用變更。請處理上方問題後重新載入。';
    lastView = undefined;
  } finally {
    commandBusy = false;
    fieldset.disabled = !lastView || lastView.settings.dictation.busy;
    reload.disabled = !!lastView?.settings.dictation.busy;
    check.disabled = !lastView || lastView.settings.dictation.busy;
  }
}

document.querySelector('#select-recovery').addEventListener('click', () => {
  recoveryText.focus();
  recoveryText.select();
});
dismissRecovery.addEventListener('click', async () => {
  if (!recovery || recoveryBusy) return;
  recoveryBusy = true;
  revision++;
  dismissRecovery.disabled = true;
  try {
    await window.__TAURI__.core.invoke('dismiss_recovery', {
      provider: recovery.provider, session: recovery.session,
    });
    await refreshRecovery();
  } catch (reason) {
    recoveryNote.textContent = String(reason);
  } finally {
    recoveryBusy = false;
    dismissRecovery.disabled = false;
  }
});

// Status carries no transcript. Fetch retained text separately only when needed;
// this display polling never delays capture, inference or delivery on the worker.
setInterval(async () => {
  if (document.hidden || commandBusy || pollBusy || recoveryBusy || !lastView) return;
  pollBusy = true;
  const requestedRevision = revision;
  try {
    const view = await window.__TAURI__.core.invoke('get_settings');
    if (requestedRevision !== revision) return;
    const wasBusy = lastView.settings.dictation.busy;
    render(view);
    fieldset.disabled = view.settings.dictation.busy;
    reload.disabled = view.settings.dictation.busy;
    check.disabled = view.settings.dictation.busy;
    if (view.settings.dictation.busy) {
      const phaseLabels = {
        preparing: '正在準備錄音…', recording: '正在錄音…',
        finalizing: '正在辨識完整錄音…', releasing: '正在等待引擎停止…',
      };
      status.textContent = phaseLabels[view.settings.dictation.phase] || '正在處理聽寫…';
    }
    else if (view.settings.dictation.failure) status.textContent = '這次聽寫未完成，請檢查引擎狀態。';
    else if (wasBusy) status.textContent = '聽寫已結束。';
    if (view.settings.dictation.has_retained_text || recovery) await refreshRecovery();
  } catch (reason) {
    if (requestedRevision !== revision) return;
    error.textContent = String(reason);
    error.hidden = false;
    fieldset.disabled = true;
    check.disabled = true;
    lastView = undefined;
  } finally {
    pollBusy = false;
  }
}, 1000);

for (const choice of choices) {
  choice.addEventListener('change', () => command('select_provider', { provider: choice.value }));
}
reload.addEventListener('click', () => command('reload_settings'));
check.addEventListener('click', () => command('refresh_providers'));
cancel.addEventListener('click', () => command('cancel_dictation'));
command('get_settings');

// Independent setup status/commands: downloads never occupy the dictation
// command queue, and setup errors must not hide retained dictation text.
const modelStatus = document.querySelector('#model-status');
const modelProgress = document.querySelector('#model-progress');
const prepareModels = document.querySelector('#prepare-models');
const cancelModels = document.querySelector('#cancel-models');
let modelBusy = false;
let modelRevision = 0;
let modelPollBusy = false;
function renderModels(view) {
  const labels = {
    not_checked: '尚未檢查模型。準備完成後可供本機引擎使用。',
    checking_installed: '正在檢查已安裝的模型…',
    downloading: '正在下載模型…', checking_download: '正在核對下載檔…',
    extracting: '正在解壓縮模型…', installing: '正在安裝及核對模型…',
    installed: '模型已通過完整檢查。辨識引擎尚待接入 App。',
    cancelled: '已取消準備。已完成的模型與原有資料仍保留。',
    failed: view.error || '模型準備未完成，請檢查網路與可用空間後重試。',
  };
  const megabytes = count => (count / 1_000_000).toFixed(1);
  let detail = '';
  if (view.busy && view.total_bytes) {
    detail = view.phase === 'extracting' ? ` 已處理 ${megabytes(view.completed_bytes)} MB`
      : ` ${megabytes(view.completed_bytes)} / ${megabytes(view.total_bytes)} MB`;
  }
  modelStatus.textContent = view.cancel_requested ? '正在取消準備並清理未完成檔案…' : (labels[view.phase] || '模型狀態未知，請重新檢查。') + detail;
  prepareModels.disabled = modelBusy || view.busy;
  cancelModels.hidden = !view.busy;
  cancelModels.disabled = modelBusy || view.cancel_requested;
  modelProgress.hidden = !view.busy;
  if (view.total_bytes && view.phase !== 'extracting') {
    modelProgress.max = view.total_bytes;
    modelProgress.value = Math.min(view.completed_bytes, view.total_bytes);
  } else {
    modelProgress.removeAttribute('value');
  }
}
async function modelCommand(name) {
  if (modelBusy) return;
  modelBusy = true;
  modelRevision++;
  prepareModels.disabled = true;
  cancelModels.disabled = true;
  try {
    const view = await window.__TAURI__.core.invoke(name);
    modelBusy = false;
    renderModels(view);
  } catch (reason) {
    modelStatus.textContent = String(reason);
    prepareModels.disabled = false;
    cancelModels.disabled = false;
  } finally {
    modelBusy = false;
  }
}
prepareModels.addEventListener('click', () => modelCommand('prepare_models'));
cancelModels.addEventListener('click', () => modelCommand('cancel_model_setup'));
setInterval(async () => {
  if (document.hidden || modelBusy || modelPollBusy) return;
  modelPollBusy = true;
  const requestedRevision = modelRevision;
  try {
    const view = await window.__TAURI__.core.invoke('get_model_setup');
    if (requestedRevision === modelRevision) renderModels(view);
  } catch (reason) {
    if (requestedRevision === modelRevision) modelStatus.textContent = String(reason);
  } finally {
    modelPollBusy = false;
  }
}, 500);
modelCommand('get_model_setup');
