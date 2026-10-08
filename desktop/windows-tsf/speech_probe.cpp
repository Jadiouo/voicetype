// Isolated TSF experiment. No installer or application uses this DLL yet.
#include "probe_api.h"
#include <atomic>
#include <cstdio>
#include <limits>
#include <new>
#include <string>
#include <utility>

namespace {
std::atomic<LONG> objects{0};
std::atomic<LONG> server_locks{0};
std::atomic<ULONGLONG> next_instance{1};
constexpr TF_PRESERVEDKEY ctrl_caps{VK_CAPITAL, TF_MOD_CONTROL};

// B diagnostics contain metadata only. The target process must explicitly
// provide a project-local absolute path; normal installed services do no IO.
void trace_probe(const char *event, ULONGLONG instance, ULONGLONG context,
                 ULONG focus_epoch, HRESULT hr, BOOL focus_match) {
  WCHAR path[1024]{};
  DWORD length = GetEnvironmentVariableW(L"VOICETYPE_TSF_B_LOG", path, 1024);
  if (length < 3 || length >= 1024 || path[1] != L':' || path[2] != L'\\') return;
  HANDLE file = CreateFileW(path, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE,
                            nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
  if (file == INVALID_HANDLE_VALUE) return;
  char line[256]{};
  int count = std::snprintf(line, sizeof(line),
      "event=%s pid=%lu tid=%lu instance=%llu context=%llu focus_epoch=%lu focus_match=%d hr=%08lx\n",
      event, GetCurrentProcessId(), GetCurrentThreadId(), instance, context,
      static_cast<unsigned long>(focus_epoch), focus_match,
      static_cast<unsigned long>(hr));
  if (count > 0 && count < static_cast<int>(sizeof(line))) {
    DWORD written = 0;
    WriteFile(file, line, static_cast<DWORD>(count), &written, nullptr);
  }
  CloseHandle(file);
}

template <class T> void release(T *&p) {
  if (p) { p->Release(); p = nullptr; }
}

class SpeechProbe final : public ITfTextInputProcessorEx,
                          public ITfThreadMgrEventSink,
                          public ITfKeyEventSink,
                          public IVoiceTypeTsfProbe {
 public:
  SpeechProbe() : instance_(next_instance.fetch_add(1)) { ++objects; }
  ~SpeechProbe() { Deactivate(); --objects; }
  STDMETHODIMP QueryInterface(REFIID iid, void **out) override {
    if (!out) return E_POINTER;
    *out = nullptr;
    if (iid == IID_IUnknown || iid == IID_ITfTextInputProcessor)
      *out = static_cast<ITfTextInputProcessor *>(this);
    else if (iid == IID_ITfTextInputProcessorEx)
      *out = static_cast<ITfTextInputProcessorEx *>(this);
    else if (iid == IID_ITfThreadMgrEventSink)
      *out = static_cast<ITfThreadMgrEventSink *>(this);
    else if (iid == IID_ITfKeyEventSink)
      *out = static_cast<ITfKeyEventSink *>(this);
    else if (iid == __uuidof(IVoiceTypeTsfProbe))
      *out = static_cast<IVoiceTypeTsfProbe *>(this);
    if (!*out) return E_NOINTERFACE;
    AddRef(); return S_OK;
  }
  STDMETHODIMP_(ULONG) AddRef() override { return ++refs_; }
  STDMETHODIMP_(ULONG) Release() override {
    ULONG count = --refs_; if (!count) delete this; return count;
  }
  STDMETHODIMP Activate(ITfThreadMgr *manager, TfClientId client) override {
    return ActivateEx(manager, client, 0);
  }
  STDMETHODIMP ActivateEx(ITfThreadMgr *manager, TfClientId client,
                          DWORD) override {
    if (!manager || manager_) return E_INVALIDARG;
    manager_ = manager; manager_->AddRef(); client_ = client;
    trace("ACTIVATE", S_OK, FALSE);
    const char *stage = "thread-source";
    ITfSource *source = nullptr;
    HRESULT hr = manager_->QueryInterface(IID_ITfSource,
                                          reinterpret_cast<void **>(&source));
    if (SUCCEEDED(hr)) {
      hr = source->AdviseSink(IID_ITfThreadMgrEventSink,
                             static_cast<ITfThreadMgrEventSink *>(this), &cookie_);
      source->Release();
    }
    if (hr == S_OK) {
      stage = "keystroke-manager";
      hr = manager_->QueryInterface(IID_ITfKeystrokeMgr,
                                    reinterpret_cast<void **>(&keys_));
    }
    if (hr == S_OK) {
      stage = "advise-nonforeground-key-sink";
      hr = keys_->AdviseKeyEventSink(client_, static_cast<ITfKeyEventSink *>(this), FALSE);
      key_advised_ = hr == S_OK;
    }
    if (hr == S_OK) {
      stage = "preserve-ctrl-caps";
      hr = keys_->PreserveKey(client_, GUID_VoiceTypeCtrlCapsProbe,
                              &ctrl_caps, nullptr, 0);
      key_preserved_ = hr == S_OK;
    }
    if (hr != S_OK) {
      std::fprintf(stderr, "TSF activation stage %s failed: 0x%08lx\n",
                   stage, static_cast<unsigned long>(hr));
      trace(hr == TF_E_ALREADY_EXISTS ? "KEY_CONFLICT" : "ACTIVATE_FAIL", hr, FALSE);
      Deactivate();
      return hr;
    }
    key_ready_ = true;
    trace("HELLO", S_OK, FALSE);
    return hr;
  }
  STDMETHODIMP Deactivate() override {
    ++epoch_; // All pending edit sessions become invalid permanently.
    key_ready_ = false;
    release(last_context_);
    HRESULT key_cleanup = S_OK;
    if (keys_) {
      if (key_preserved_) {
        HRESULT hr = keys_->UnpreserveKey(GUID_VoiceTypeCtrlCapsProbe, &ctrl_caps);
        trace("UNPRESERVE", hr, FALSE);
        if (hr != S_OK) key_cleanup = hr;
      }
      if (key_advised_) {
        HRESULT hr = keys_->UnadviseKeyEventSink(client_);
        trace("UNADVISE_KEY", hr, FALSE);
        if (hr != S_OK) key_cleanup = hr;
      }
      key_preserved_ = key_advised_ = false;
      release(keys_);
    }
    if (manager_ && cookie_ != TF_INVALID_COOKIE) {
      ITfSource *source = nullptr;
      if (SUCCEEDED(manager_->QueryInterface(IID_ITfSource,
                    reinterpret_cast<void **>(&source)))) {
        source->UnadviseSink(cookie_); source->Release();
      }
      cookie_ = TF_INVALID_COOKIE;
    }
    if (manager_) trace("DEACTIVATE", key_cleanup, FALSE);
    release(manager_); client_ = TF_CLIENTID_NULL;
    return key_cleanup;
  }
  STDMETHODIMP OnInitDocumentMgr(ITfDocumentMgr *) override { return S_OK; }
  STDMETHODIMP OnUninitDocumentMgr(ITfDocumentMgr *) override {
    invalidate_focus(); return S_OK;
  }
  STDMETHODIMP OnSetFocus(ITfDocumentMgr *, ITfDocumentMgr *) override {
    invalidate_focus(); return S_OK;
  }
  STDMETHODIMP OnPushContext(ITfContext *) override { invalidate_focus(); return S_OK; }
  STDMETHODIMP OnPopContext(ITfContext *) override { invalidate_focus(); return S_OK; }

  STDMETHODIMP OnSetFocus(BOOL foreground) override {
    trace(foreground ? "KEY_FOCUS_GAIN" : "KEY_FOCUS_LOSS", S_OK, FALSE);
    return S_OK;
  }
  STDMETHODIMP OnTestKeyDown(ITfContext *, WPARAM, LPARAM, BOOL *eaten) override {
    return leave_key(eaten);
  }
  STDMETHODIMP OnTestKeyUp(ITfContext *, WPARAM, LPARAM, BOOL *eaten) override {
    return leave_key(eaten);
  }
  STDMETHODIMP OnKeyDown(ITfContext *, WPARAM, LPARAM, BOOL *eaten) override {
    return leave_key(eaten);
  }
  STDMETHODIMP OnKeyUp(ITfContext *, WPARAM, LPARAM, BOOL *eaten) override {
    return leave_key(eaten);
  }
  STDMETHODIMP OnPreservedKey(ITfContext *context, REFGUID guid,
                              BOOL *eaten) override {
    if (!eaten) return E_POINTER;
    *eaten = FALSE; // Diagnostic only: do not alter Caps or the existing IME.
    if (!key_ready_ || !context || guid != GUID_VoiceTypeCtrlCapsProbe)
      return S_OK;
    ITfDocumentMgr *focused = nullptr;
    ITfContext *top = nullptr;
    IUnknown *incoming = nullptr, *current = nullptr;
    HRESULT hr = manager_->GetFocus(&focused);
    if (hr == S_OK && focused) hr = focused->GetTop(&top);
    if (hr == S_OK && top) hr = top->QueryInterface(IID_IUnknown,
                                                    reinterpret_cast<void **>(&current));
    if (hr == S_OK) hr = context->QueryInterface(IID_IUnknown,
                                                reinterpret_cast<void **>(&incoming));
    const BOOL same = hr == S_OK && incoming && incoming == current;
    if (same) {
      if (incoming != last_context_) {
        release(last_context_);
        last_context_ = incoming;
        last_context_->AddRef();
        ++context_serial_;
      }
      ++key_callbacks_;
    }
    trace(same ? "PRESERVED" : "PRESERVED_REJECTED", hr, same);
    release(incoming); release(current); release(top); release(focused);
    return S_OK;
  }

  class Edit final : public ITfEditSession {
   public:
    Edit(SpeechProbe *owner, ITfContext *context, std::wstring text, ULONG epoch)
        : owner_(owner), context_(context), text_(std::move(text)), epoch_(epoch) {
      owner_->AddRef(); context_->AddRef(); ++objects;
    }
    ~Edit() { context_->Release(); owner_->Release(); --objects; }
    STDMETHODIMP QueryInterface(REFIID iid, void **out) override {
      if (!out) return E_POINTER;
      *out = (iid == IID_IUnknown || iid == IID_ITfEditSession)
          ? static_cast<ITfEditSession *>(this) : nullptr;
      if (!*out) return E_NOINTERFACE;
      AddRef(); return S_OK;
    }
    STDMETHODIMP_(ULONG) AddRef() override { return ++refs_; }
    STDMETHODIMP_(ULONG) Release() override {
      ULONG count = --refs_; if (!count) delete this; return count;
    }
    STDMETHODIMP DoEditSession(TfEditCookie cookie) override {
      HRESULT hr = owner_->insert(context_, cookie, text_, epoch_);
      owner_->result_ = hr;
      return hr;
    }
   private:
    std::atomic<ULONG> refs_{1};
    SpeechProbe *owner_;
    ITfContext *context_;
    std::wstring text_;
    ULONG epoch_;
  };

  STDMETHODIMP Commit(ITfContext *context, const WCHAR *text, LONG length) override {
    if (!manager_ || !context || !text || length <= 0 || length > 32768 ||
        result_ == E_PENDING) return E_INVALIDARG;
    ITfDocumentMgr *focused = nullptr;
    ITfContext *top = nullptr;
    HRESULT hr = manager_->GetFocus(&focused);
    if (SUCCEEDED(hr) && focused) hr = focused->GetTop(&top);
    if (FAILED(hr) || !top || top != context) {
      release(top); release(focused); return E_ACCESSDENIED;
    }
    release(top); release(focused);
    result_ = E_PENDING;
    auto *edit = new (std::nothrow) Edit(this, context,
                                         std::wstring(text, text + length), epoch_);
    if (!edit) { result_ = E_OUTOFMEMORY; return result_; }
    HRESULT session = E_FAIL;
    hr = context->RequestEditSession(client_, edit, TF_ES_ASYNC | TF_ES_READWRITE,
                                     &session);
    edit->Release();
    if (FAILED(hr)) result_ = hr;
    else if (FAILED(session)) result_ = session;
    return hr;
  }
  STDMETHODIMP Result(HRESULT *result, LONG *attempts) override {
    if (!result || !attempts) return E_POINTER;
    *result = result_; *attempts = attempts_; return S_OK;
  }
  STDMETHODIMP KeySnapshot(LONG *callbacks, ULONGLONG *context_serial,
                            BOOL *nonforeground) override {
    if (!callbacks || !context_serial || !nonforeground) return E_POINTER;
    *callbacks = key_callbacks_;
    *context_serial = context_serial_;
    *nonforeground = key_advised_ && key_ready_;
    return S_OK;
  }

 private:
  static HRESULT leave_key(BOOL *eaten) {
    if (!eaten) return E_POINTER;
    *eaten = FALSE;
    return S_OK;
  }
  void trace(const char *event, HRESULT hr, BOOL focus_match) const {
    trace_probe(event, instance_, context_serial_, focus_epoch_, hr, focus_match);
  }
  void invalidate_focus() {
    ++epoch_;
    ++focus_epoch_;
    release(last_context_);
    trace("FOCUS_EPOCH", S_OK, FALSE);
  }
  HRESULT insert(ITfContext *context, TfEditCookie cookie,
                 const std::wstring &text, ULONG epoch) {
    if (!manager_ || epoch_ != epoch) return E_ACCESSDENIED;
    ITfDocumentMgr *focused = nullptr;
    ITfContext *top = nullptr;
    HRESULT hr = manager_->GetFocus(&focused);
    if (SUCCEEDED(hr) && focused) hr = focused->GetTop(&top);
    bool same = SUCCEEDED(hr) && top == context && epoch_ == epoch;
    release(top); release(focused);
    if (!same) return E_ACCESSDENIED;
    TF_SELECTION selection{};
    ULONG fetched = 0;
    hr = context->GetSelection(cookie, TF_DEFAULT_SELECTION, 1, &selection, &fetched);
    if (FAILED(hr) || fetched != 1 || !selection.range) return FAILED(hr) ? hr : E_FAIL;
    if (epoch_ == epoch) {
      ++attempts_;
      hr = selection.range->SetText(cookie, 0, text.data(),
                                    static_cast<LONG>(text.size()));
    } else hr = E_ACCESSDENIED;
    selection.range->Release();
    return hr;
  }
  std::atomic<ULONG> refs_{1};
  ITfThreadMgr *manager_ = nullptr;
  ITfKeystrokeMgr *keys_ = nullptr;
  IUnknown *last_context_ = nullptr;
  TfClientId client_ = TF_CLIENTID_NULL;
  DWORD cookie_ = TF_INVALID_COOKIE;
  ULONG epoch_ = 0;
  ULONG focus_epoch_ = 0;
  ULONGLONG instance_ = 0;
  ULONGLONG context_serial_ = 0;
  LONG key_callbacks_ = 0;
  bool key_advised_ = false;
  bool key_preserved_ = false;
  bool key_ready_ = false;
  HRESULT result_ = E_ABORT;
  LONG attempts_ = 0;
};

class Factory final : public IClassFactory {
 public:
  Factory() { ++objects; }
  ~Factory() { --objects; }
  STDMETHODIMP QueryInterface(REFIID iid, void **out) override {
    if (!out) return E_POINTER;
    *out = (iid == IID_IUnknown || iid == IID_IClassFactory)
        ? static_cast<IClassFactory *>(this) : nullptr;
    if (!*out) return E_NOINTERFACE;
    AddRef(); return S_OK;
  }
  STDMETHODIMP_(ULONG) AddRef() override { return ++refs_; }
  STDMETHODIMP_(ULONG) Release() override {
    ULONG count = --refs_; if (!count) delete this; return count;
  }
  STDMETHODIMP CreateInstance(IUnknown *outer, REFIID iid, void **out) override {
    if (!out) return E_POINTER;
    *out = nullptr;
    if (outer) return CLASS_E_NOAGGREGATION;
    auto *service = new (std::nothrow) SpeechProbe();
    if (!service) return E_OUTOFMEMORY;
    HRESULT hr = service->QueryInterface(iid, out);
    service->Release(); return hr;
  }
  STDMETHODIMP LockServer(BOOL lock) override {
    if (lock) ++server_locks; else --server_locks; return S_OK;
  }
 private:
  std::atomic<ULONG> refs_{1};
};
} // namespace

STDAPI DllGetClassObject(
    REFCLSID clsid, REFIID iid, void **out) {
  if (!out) return E_POINTER;
  *out = nullptr;
  if (clsid != CLSID_VoiceTypeSpeechProbe) return CLASS_E_CLASSNOTAVAILABLE;
  auto *factory = new (std::nothrow) Factory();
  if (!factory) return E_OUTOFMEMORY;
  HRESULT hr = factory->QueryInterface(iid, out);
  factory->Release(); return hr;
}
STDAPI DllCanUnloadNow() {
  return objects.load() == 0 && server_locks.load() == 0 ? S_OK : S_FALSE;
}
