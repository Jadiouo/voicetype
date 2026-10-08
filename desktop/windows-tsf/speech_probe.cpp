// Isolated TSF experiment. No installer or application uses this DLL yet.
#include "probe_api.h"
#include <atomic>
#include <limits>
#include <new>
#include <string>
#include <utility>

namespace {
std::atomic<LONG> objects{0};
std::atomic<LONG> server_locks{0};

template <class T> void release(T *&p) {
  if (p) { p->Release(); p = nullptr; }
}

class SpeechProbe final : public ITfTextInputProcessorEx,
                          public ITfThreadMgrEventSink,
                          public IVoiceTypeTsfProbe {
 public:
  SpeechProbe() { ++objects; }
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
    ITfSource *source = nullptr;
    HRESULT hr = manager_->QueryInterface(IID_ITfSource,
                                          reinterpret_cast<void **>(&source));
    if (SUCCEEDED(hr)) {
      hr = source->AdviseSink(IID_ITfThreadMgrEventSink,
                             static_cast<ITfThreadMgrEventSink *>(this), &cookie_);
      source->Release();
    }
    if (FAILED(hr)) { release(manager_); client_ = TF_CLIENTID_NULL; }
    return hr;
  }
  STDMETHODIMP Deactivate() override {
    ++epoch_; // All pending edit sessions become invalid permanently.
    if (manager_ && cookie_ != TF_INVALID_COOKIE) {
      ITfSource *source = nullptr;
      if (SUCCEEDED(manager_->QueryInterface(IID_ITfSource,
                    reinterpret_cast<void **>(&source)))) {
        source->UnadviseSink(cookie_); source->Release();
      }
      cookie_ = TF_INVALID_COOKIE;
    }
    release(manager_); client_ = TF_CLIENTID_NULL;
    return S_OK;
  }
  STDMETHODIMP OnInitDocumentMgr(ITfDocumentMgr *) override { return S_OK; }
  STDMETHODIMP OnUninitDocumentMgr(ITfDocumentMgr *) override { ++epoch_; return S_OK; }
  STDMETHODIMP OnSetFocus(ITfDocumentMgr *, ITfDocumentMgr *) override {
    ++epoch_; return S_OK;
  }
  STDMETHODIMP OnPushContext(ITfContext *) override { ++epoch_; return S_OK; }
  STDMETHODIMP OnPopContext(ITfContext *) override { ++epoch_; return S_OK; }

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

 private:
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
  TfClientId client_ = TF_CLIENTID_NULL;
  DWORD cookie_ = TF_INVALID_COOKIE;
  ULONG epoch_ = 0;
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
