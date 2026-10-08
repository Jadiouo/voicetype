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
const loadRuntime = document.querySelector('#load-local-runtime');
const unloadRuntime = document.querySelector('#unload-local-runtime');
const enableInput = document.querySelector('#enable-local-input');
const spellingEnabled = document.querySelector('#spelling-enabled');
spellingEnabled.addEventListener('change', () => command('set_spelling_enabled', { enabled: spellingEnabled.checked }));
let moduleBusy = false;
const installModule = document.querySelector('#install-input-module');
const restoreModule = document.querySelector('#restore-input-module');

function runtimeControls(view) {
  spellingEnabled.disabled = commandBusy || !view || view.local_runtime !== 'inactive' || view.settings.dictation.busy;
  if (view) spellingEnabled.checked = view.settings.spelling_enabled;
  const spellingNotes = {
    inactive: view?.settings.spelling_enabled ? '校正已選取；載入引擎時會核對。' : '校正已關閉。',
    disabled: '本次引擎未啟用校正。',
    ready: 'CPU 校正器已就緒。',
    unavailable: 'CPU 校正器本次不可用；仍可使用本機語音辨識，送字時保留原文。請卸載後檢查安裝包再載入。',
  };
  document.querySelector('#spelling-runtime-note').textContent = spellingNotes[view?.spelling_runtime] || '校正器會在載入引擎時檢查。';
  document.querySelector('#input-module-setup').hidden = !view?.local_runtime_bundled;
  installModule.disabled = restoreModule.disabled = moduleBusy || commandBusy
    || !view?.local_runtime_bundled || view.input_requested || view.settings.dictation.busy;
  const loaded = ['waiting_for_input', 'ready'].includes(view?.local_runtime);
  loadRuntime.disabled = commandBusy || !view?.local_runtime_bundled || loaded
    || view.settings.dictation.busy || view.settings.selected_provider !== 'local';
  unloadRuntime.hidden = !loaded && view?.local_runtime !== 'failed';
  unloadRuntime.disabled = commandBusy || !!view?.settings.dictation.busy;
  enableInput.disabled = commandBusy || !view?.local_runtime_bundled || !loaded
    || view?.input_requested || !!view?.settings.dictation.busy;
  document.querySelector('#runtime-note').textContent = !view?.local_runtime_bundled
    ? '這個平台的引擎封裝仍在準備中。'
    : view.local_runtime === 'ready' ? '按住 Ctrl＋Alt 說話，放開後送字；Esc 取消。停用或結束 App 後回到原服務。'
    : view.input_requested ? '等待 Fcitx 接管：原聽寫結束後才切換。若持續等待，請確認已載入本版 Fcitx 模組；舊的使用者模組可能遮住安裝包版本。'
    : loaded ? '引擎已載入。按「啟用本機聽寫」後，Fcitx 才會在空閒時接管快捷鍵。'
    : '載入時會核對完整模型並使用 CPU。這一步不會開始錄音。';
}

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
  runtimeControls(view);
  cancel.hidden = !view.settings.dictation.busy;
  cancel.disabled = view.settings.dictation.phase === 'releasing';
  for (const choice of choices) {
    choice.checked = choice.value === view.settings.selected_provider;
  }
  const labels = {
    not_connected: '準備中 · 尚未連接辨識引擎',
    service_available: '本機服務有回應 · 尚未啟用 App 聽寫',
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
  runtimeControls(lastView);
  revision++;
  fieldset.disabled = true;
  reload.disabled = true;
  check.disabled = true;
  cancel.disabled = true;
  error.hidden = true;
  status.textContent = name === 'select_provider' ? '正在儲存…'
    : name === 'load_local_runtime' ? '正在核對模型並載入本機引擎，首次載入可能需要一些時間…'
    : name === 'unload_local_runtime' ? '正在卸載本機引擎…'
    : name === 'enable_local_input' ? '正在要求 Fcitx 接管…'
    : name === 'cancel_dictation' ? '正在取消聽寫…'
    : name === 'refresh_providers' ? '正在檢查服務…' : '正在讀取設定…';
  try {
    if (!window.__TAURI__) throw new Error('請透過已安裝的 VoiceType App 開啟設定。');
    const view = await window.__TAURI__.core.invoke(name, args);
    render(view);
    await refreshRecovery().catch(reason => { recoveryNote.textContent = String(reason); });
    status.textContent = name === 'select_provider'
      ? '偏好已儲存。請確認所選方式的就緒狀態。'
      : name === 'load_local_runtime' ? (view.spelling_runtime === 'unavailable'
        ? '本機辨識引擎已載入；CPU 校正器本次不可用。仍可啟用本機聽寫。'
        : '引擎已載入，可啟用本機聽寫。尚未開始錄音。')
      : name === 'enable_local_input' ? '已要求接管，請等待顯示「本機引擎與輸入法已連接」。'
      : name === 'unload_local_runtime' ? '本機引擎已卸載。'
      : name === 'cancel_dictation' ? '已要求取消，正在等待引擎停止。'
      : name === 'refresh_providers' ? '已檢查服務。此操作不會啟動錄音。'
      : '已讀取你的偏好設定。';
  } catch (reason) {
    if (lastView) render(lastView);
    error.textContent = String(reason);
    error.hidden = false;
    status.textContent = '尚未套用變更。請處理上方問題後重新載入。';
    if (!(name === 'reload_settings' && String(reason).includes('請先停用並卸載引擎'))) {
      lastView = undefined;
    }
  } finally {
    commandBusy = false;
    runtimeControls(lastView);
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
loadRuntime.addEventListener('click', () => command('load_local_runtime'));
unloadRuntime.addEventListener('click', () => command('unload_local_runtime'));
enableInput.addEventListener('click', () => command('enable_local_input'));
async function configureModule(restore) {
  if (moduleBusy || commandBusy) return;
  moduleBusy = true;
  runtimeControls(lastView);
  const note = document.querySelector('#input-module-status');
  note.textContent = restore ? '正在還原模組設定…' : '正在核對並安裝模組…';
  try {
    note.textContent = await window.__TAURI__.core.invoke('configure_input_module', { restore });
  } catch (reason) { note.textContent = String(reason); }
  finally { moduleBusy = false; runtimeControls(lastView); }
}
installModule.addEventListener('click', () => configureModule(false));
restoreModule.addEventListener('click', () => configureModule(true));
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
    installed: '模型已通過完整檢查，可供本機引擎載入。',
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
