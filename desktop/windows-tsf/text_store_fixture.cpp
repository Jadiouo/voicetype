// A deliberately small ACP host, used only by the real-Msctf CTest probe.
// Unsupported host capabilities return E_NOTIMPL; no production app uses it.
#include "probe_api.h"
#include <textstor.h>
#include <ocidl.h>
#include <algorithm>
#include <atomic>
#include <cwchar>
#include <string>

class TextStore final : public ITextStoreACP {
 public:
  STDMETHODIMP QueryInterface(REFIID iid, void **out) override {
    if (!out) return E_POINTER;
    *out = (iid == IID_IUnknown || iid == IID_ITextStoreACP)
        ? static_cast<ITextStoreACP *>(this) : nullptr;
    if (!*out) return E_NOINTERFACE;
    AddRef(); return S_OK;
  }
  STDMETHODIMP_(ULONG) AddRef() override { return ++refs_; }
  STDMETHODIMP_(ULONG) Release() override {
    ULONG n = --refs_; if (!n) delete this; return n;
  }
  const std::wstring &text() const { return text_; }
  LONG mutations() const { return mutations_; }

  STDMETHODIMP AdviseSink(REFIID iid, IUnknown *unknown, DWORD) override {
    if (iid != IID_ITextStoreACPSink || !unknown) return E_INVALIDARG;
    if (sink_) return CONNECT_E_ADVISELIMIT;
    return unknown->QueryInterface(IID_ITextStoreACPSink,
                                   reinterpret_cast<void **>(&sink_));
  }
  STDMETHODIMP UnadviseSink(IUnknown *unknown) override {
    if (!sink_ || !unknown) return CONNECT_E_NOCONNECTION;
    IUnknown *registered = nullptr;
    IUnknown *requested = nullptr;
    sink_->QueryInterface(IID_IUnknown, reinterpret_cast<void **>(&registered));
    unknown->QueryInterface(IID_IUnknown, reinterpret_cast<void **>(&requested));
    bool same = registered && registered == requested;
    if (registered) registered->Release();
    if (requested) requested->Release();
    if (!same) return CONNECT_E_NOCONNECTION;
    sink_->Release(); sink_ = nullptr; return S_OK;
  }
  STDMETHODIMP RequestLock(DWORD flags, HRESULT *result) override {
    if (!result) return E_POINTER;
    if (!sink_) return E_UNEXPECTED;
    if (locked_) {
      if (flags & TS_LF_SYNC) { *result = TS_E_SYNCHRONOUS; return S_OK; }
      pending_ = (flags & TS_LF_READWRITE) ? TS_LF_READWRITE : TS_LF_READ;
      *result = TS_S_ASYNC;
      return S_OK;
    }
    locked_ = flags & TS_LF_READWRITE ? TS_LF_READWRITE : TS_LF_READ;
    *result = sink_->OnLockGranted(locked_);
    locked_ = 0;
    if (pending_) {
      DWORD next = pending_;
      pending_ = 0;
      locked_ = next;
      sink_->OnLockGranted(next);
      locked_ = 0;
    }
    return S_OK;
  }
  STDMETHODIMP GetStatus(TS_STATUS *status) override {
    if (!status) return E_POINTER;
    status->dwDynamicFlags = 0; status->dwStaticFlags = 0; return S_OK;
  }
  STDMETHODIMP QueryInsert(LONG begin, LONG end, ULONG,
                           LONG *out_begin, LONG *out_end) override {
    if (!out_begin || !out_end) return E_POINTER;
    if (begin < 0 || end < begin || end > static_cast<LONG>(text_.size()))
      return E_INVALIDARG;
    *out_begin = begin; *out_end = end; return S_OK;
  }
  STDMETHODIMP GetSelection(ULONG index, ULONG count,
                            TS_SELECTION_ACP *selections, ULONG *fetched) override {
    if (!fetched) return E_POINTER;
    *fetched = 0;
    if (!read_lock()) return TS_E_NOLOCK;
    if (!selections || count == 0) return E_INVALIDARG;
    if (index != TS_DEFAULT_SELECTION && index != 0) return S_OK;
    selections[0].acpStart = selection_begin_;
    selections[0].acpEnd = selection_end_;
    selections[0].style.ase = TS_AE_END;
    selections[0].style.fInterimChar = FALSE;
    *fetched = 1; return S_OK;
  }
  STDMETHODIMP SetSelection(ULONG count, const TS_SELECTION_ACP *selections) override {
    if (!write_lock()) return TS_E_NOLOCK;
    if (count != 1 || !selections || selections[0].acpStart < 0 ||
        selections[0].acpEnd < selections[0].acpStart ||
        selections[0].acpEnd > static_cast<LONG>(text_.size())) return E_INVALIDARG;
    selection_begin_ = selections[0].acpStart;
    selection_end_ = selections[0].acpEnd;
    return S_OK;
  }
  STDMETHODIMP GetText(LONG begin, LONG end, WCHAR *plain, ULONG plain_max,
                       ULONG *plain_copied, TS_RUNINFO *runs, ULONG run_max,
                       ULONG *runs_copied, LONG *next) override {
    if (!plain_copied || !runs_copied || !next) return E_POINTER;
    *plain_copied = *runs_copied = 0;
    if (!read_lock()) return TS_E_NOLOCK;
    if (end == -1) end = static_cast<LONG>(text_.size());
    if (begin < 0 || end < begin || end > static_cast<LONG>(text_.size()))
      return E_INVALIDARG;
    ULONG count = std::min<ULONG>(plain_max, static_cast<ULONG>(end - begin));
    if (count && !plain) return E_POINTER;
    if (count) std::wmemcpy(plain, text_.data() + begin, count);
    *plain_copied = count; *next = begin + static_cast<LONG>(count);
    if (runs && run_max && count) {
      runs[0].type = TS_RT_PLAIN; runs[0].uCount = count; *runs_copied = 1;
    }
    return S_OK;
  }
  STDMETHODIMP SetText(DWORD, LONG begin, LONG end, const WCHAR *plain,
                       ULONG count, TS_TEXTCHANGE *change) override {
    if (!write_lock()) return TS_E_NOLOCK;
    if (!change || (!plain && count) || begin < 0 || end < begin ||
        end > static_cast<LONG>(text_.size())) return E_INVALIDARG;
    text_.replace(begin, end - begin, plain ? plain : L"", count);
    change->acpStart = begin; change->acpOldEnd = end;
    change->acpNewEnd = begin + static_cast<LONG>(count);
    selection_begin_ = selection_end_ = change->acpNewEnd;
    ++mutations_;
    return S_OK;
  }
  STDMETHODIMP InsertTextAtSelection(DWORD flags, const WCHAR *plain, ULONG count,
                                     LONG *begin, LONG *end,
                                     TS_TEXTCHANGE *change) override {
    if (!read_lock()) return TS_E_NOLOCK;
    LONG b = selection_begin_, e = selection_end_;
    if (begin) *begin = b;
    if (end) *end = b + static_cast<LONG>(count);
    if (flags & TS_IAS_QUERYONLY) return S_OK;
    if (!write_lock()) return TS_E_NOLOCK;
    return SetText(0, b, e, plain, count, change);
  }
  STDMETHODIMP GetEndACP(LONG *end) override {
    if (!end) return E_POINTER;
    if (!read_lock()) return TS_E_NOLOCK;
    *end = static_cast<LONG>(text_.size()); return S_OK;
  }
  STDMETHODIMP GetActiveView(TsViewCookie *view) override {
    if (!view) return E_POINTER;
    *view = 1; return S_OK;
  }
  STDMETHODIMP GetWnd(TsViewCookie, HWND *window) override {
    if (!window) return E_POINTER;
    *window = nullptr; return S_OK;
  }
  STDMETHODIMP GetTextExt(TsViewCookie, LONG, LONG, RECT *, BOOL *) override { return E_NOTIMPL; }
  STDMETHODIMP GetScreenExt(TsViewCookie, RECT *) override { return E_NOTIMPL; }
  STDMETHODIMP GetACPFromPoint(TsViewCookie, const POINT *, DWORD, LONG *) override { return E_NOTIMPL; }
  STDMETHODIMP GetFormattedText(LONG, LONG, IDataObject **) override { return E_NOTIMPL; }
  STDMETHODIMP GetEmbedded(LONG, REFGUID, REFIID, IUnknown **) override { return E_NOTIMPL; }
  STDMETHODIMP QueryInsertEmbedded(const GUID *, const FORMATETC *, BOOL *result) override {
    if (!result) return E_POINTER;
    *result = FALSE; return S_OK;
  }
  STDMETHODIMP InsertEmbedded(DWORD, LONG, LONG, IDataObject *, TS_TEXTCHANGE *) override {
    return E_NOTIMPL;
  }
  STDMETHODIMP InsertEmbeddedAtSelection(DWORD, IDataObject *, LONG *, LONG *,
                                         TS_TEXTCHANGE *) override { return E_NOTIMPL; }
  STDMETHODIMP RequestSupportedAttrs(DWORD, ULONG, const TS_ATTRID *) override { return S_OK; }
  STDMETHODIMP RequestAttrsAtPosition(LONG, ULONG, const TS_ATTRID *, DWORD) override { return S_OK; }
  STDMETHODIMP RequestAttrsTransitioningAtPosition(LONG, ULONG, const TS_ATTRID *, DWORD) override {
    return S_OK;
  }
  STDMETHODIMP FindNextAttrTransition(LONG, LONG, ULONG, const TS_ATTRID *, DWORD,
                                      LONG *, BOOL *, LONG *) override { return E_NOTIMPL; }
  STDMETHODIMP RetrieveRequestedAttrs(ULONG, TS_ATTRVAL *, ULONG *count) override {
    if (!count) return E_POINTER;
    *count = 0; return S_OK;
  }
 private:
  ~TextStore() { if (sink_) sink_->Release(); }
  bool read_lock() const { return locked_ == TS_LF_READ || locked_ == TS_LF_READWRITE; }
  bool write_lock() const { return locked_ == TS_LF_READWRITE; }
  std::atomic<ULONG> refs_{1};
  ITextStoreACPSink *sink_ = nullptr;
  DWORD locked_ = 0;
  DWORD pending_ = 0;
  LONG selection_begin_ = 0, selection_end_ = 0;
  LONG mutations_ = 0;
  std::wstring text_;
};

extern "C" ITextStoreACP *CreateFixtureTextStore() { return new TextStore(); }
extern "C" LONG FixtureMutationCount(ITextStoreACP *store) {
  return static_cast<TextStore *>(store)->mutations();
}
extern "C" const WCHAR *FixtureText(ITextStoreACP *store) {
  return static_cast<TextStore *>(store)->text().c_str();
}
