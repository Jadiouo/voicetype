const fieldset = document.querySelector('#providers');
const error = document.querySelector('#error');
const status = document.querySelector('#status');
const reload = document.querySelector('#reload');
const check = document.querySelector('#check-providers');
const choices = [...document.querySelectorAll('input[name="provider"]')];
let lastView;

function render(view) {
  lastView = view;
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
  document.querySelector('#version').textContent = `開發預覽 ${view.settings.version}`;
  document.querySelector('#tray-note').textContent = view.tray_available
    ? '可從系統圖示開啟此視窗。關閉視窗會結束預覽 App。'
    : '此桌面未提供常駐圖示。關閉視窗會結束預覽 App。';
}

async function command(name, args = {}) {
  fieldset.disabled = true;
  reload.disabled = true;
  check.disabled = true;
  error.hidden = true;
  status.textContent = name === 'select_provider' ? '正在儲存…'
    : name === 'refresh_providers' ? '正在檢查服務…' : '正在讀取設定…';
  try {
    if (!window.__TAURI__) throw new Error('請透過已安裝的 VoiceType App 開啟設定。');
    const view = await window.__TAURI__.core.invoke(name, args);
    render(view);
    status.textContent = name === 'select_provider'
      ? '偏好已儲存。現有聽寫方式尚未變更。'
      : name === 'refresh_providers' ? '已檢查服務。此操作不會啟動錄音。'
      : '已讀取你的偏好設定。';
  } catch (reason) {
    if (lastView) render(lastView);
    error.textContent = String(reason);
    error.hidden = false;
    status.textContent = '尚未套用變更。請處理上方問題後重新載入。';
    lastView = undefined;
  } finally {
    fieldset.disabled = !lastView;
    reload.disabled = false;
    check.disabled = !lastView;
  }
}

for (const choice of choices) {
  choice.addEventListener('change', () => command('select_provider', { provider: choice.value }));
}
reload.addEventListener('click', () => command('reload_settings'));
check.addEventListener('click', () => command('refresh_providers'));
command('get_settings');
