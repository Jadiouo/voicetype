// Harness-controlled activation, not proof that Windows loads a speech profile.
#include "probe_api.h"
#include <textstor.h>
#include <windows.h>
#include <cstdio>
#include <cwchar>
#include <initializer_list>

extern "C" ITextStoreACP *CreateFixtureTextStore();
extern "C" LONG FixtureMutationCount(ITextStoreACP *);
extern "C" const WCHAR *FixtureText(ITextStoreACP *);

using GetFactory = HRESULT(__stdcall *)(REFCLSID, REFIID, void **);
using CanUnload = HRESULT(__stdcall *)();
static bool check(HRESULT hr, const char *step) {
  if (SUCCEEDED(hr)) return true;
  std::fprintf(stderr, "%s failed: 0x%08lx\n", step,
               static_cast<unsigned long>(hr));
  return false;
}

int wmain(int argc, wchar_t **argv) {
  const bool preserved = argc == 3 && wcscmp(argv[2], L"--preserved") == 0;
  if (argc != 2 && !preserved) return 2;
  if (!check(CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED), "CoInitializeEx"))
    return 2;
  int exit_code = 1;
  HMODULE module = LoadLibraryExW(argv[1], nullptr,
      LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
  if (!module) {
    std::fprintf(stderr, "LoadLibraryExW failed: %lu\n", GetLastError());
    CoUninitialize(); return 1;
  }
  auto get_factory = reinterpret_cast<GetFactory>(GetProcAddress(module, "DllGetClassObject"));
  auto can_unload = reinterpret_cast<CanUnload>(GetProcAddress(module, "DllCanUnloadNow"));
  IClassFactory *factory = nullptr;
  ITfTextInputProcessor *service = nullptr;
  IVoiceTypeTsfProbe *probe = nullptr;
  ITfTextInputProcessorEx *service_ex = nullptr;
  ITfThreadMgrEventSink *event_sink = nullptr;
  IUnknown *canonical = nullptr, *other = nullptr;
  ITfThreadMgr *manager = nullptr;
  ITfKeystrokeMgr *keys = nullptr;
  ITfDocumentMgr *doc = nullptr;
  ITfContext *context = nullptr;
  ITextStoreACP *store = nullptr;
  TfClientId client = TF_CLIENTID_NULL;
  bool manager_active = false, service_active = false, pushed = false;
  bool server_locked = false;
  do {
    if (!get_factory || !can_unload ||
        !check(get_factory(CLSID_VoiceTypeSpeechProbe, IID_IClassFactory,
                           reinterpret_cast<void **>(&factory)), "DllGetClassObject")) break;
    if (can_unload() != S_FALSE) {
      std::fputs("factory reference did not hold DLL\n", stderr); break;
    }
    if (!check(factory->QueryInterface(IID_IUnknown,
                    reinterpret_cast<void **>(&other)), "QI factory identity")) break;
    if (other != static_cast<IUnknown *>(factory)) {
      std::fputs("factory has different IUnknown identity\n", stderr); break;
    }
    other->Release(); other = nullptr;
    ULONG factory_extra = factory->AddRef();
    if (factory->Release() + 1 != factory_extra) {
      std::fputs("factory AddRef/Release count mismatch\n", stderr); break;
    }
    if (!check(factory->LockServer(TRUE), "LockServer(TRUE)")) break;
    server_locked = true;
    factory->Release(); factory = nullptr;
    if (can_unload() != S_FALSE) {
      std::fputs("server lock did not hold DLL\n", stderr); break;
    }
    if (!check(get_factory(CLSID_VoiceTypeSpeechProbe, IID_IClassFactory,
                           reinterpret_cast<void **>(&factory)), "recreate factory")) break;
    if (!check(factory->LockServer(FALSE), "LockServer(FALSE)")) break;
    server_locked = false;
    factory->Release(); factory = nullptr;
    if (can_unload() != S_OK) {
      std::fputs("balanced server lock left DLL referenced\n", stderr); break;
    }
    if (!check(get_factory(CLSID_VoiceTypeSpeechProbe, IID_IClassFactory,
                           reinterpret_cast<void **>(&factory)), "factory after unlock")) break;
    if (!check(factory->CreateInstance(nullptr, IID_ITfTextInputProcessor,
                    reinterpret_cast<void **>(&service)), "CreateInstance")) break;
    if (!check(service->QueryInterface(IID_IUnknown,
                    reinterpret_cast<void **>(&canonical)), "QI canonical IUnknown") ||
        !check(service->QueryInterface(IID_ITfTextInputProcessorEx,
                    reinterpret_cast<void **>(&service_ex)), "QI processor Ex") ||
        !check(service->QueryInterface(IID_ITfThreadMgrEventSink,
                    reinterpret_cast<void **>(&event_sink)), "QI event sink")) break;
    bool identity_ok = true;
    for (IUnknown *iface : {static_cast<IUnknown *>(service_ex),
                            static_cast<IUnknown *>(event_sink)}) {
      if (!check(iface->QueryInterface(IID_IUnknown,
                    reinterpret_cast<void **>(&other)), "QI interface identity")) {
        identity_ok = false; break;
      }
      if (other != canonical) {
        std::fputs("COM interfaces have different IUnknown identity\n", stderr);
        identity_ok = false;
      }
      other->Release(); other = nullptr;
      if (!identity_ok) break;
    }
    if (!identity_ok) break;
    ULONG with_extra = service->AddRef();
    if (service->Release() + 1 != with_extra) {
      std::fputs("AddRef/Release count mismatch\n", stderr); break;
    }
    if (!check(service->QueryInterface(__uuidof(IVoiceTypeTsfProbe),
                    reinterpret_cast<void **>(&probe)), "QI probe")) break;
    if (!check(probe->QueryInterface(IID_IUnknown,
                    reinterpret_cast<void **>(&other)), "QI probe identity")) break;
    if (other != canonical) {
      std::fputs("private probe has different IUnknown identity\n", stderr); break;
    }
    other->Release(); other = nullptr;
    canonical->Release(); canonical = nullptr;
    service_ex->Release(); service_ex = nullptr;
    event_sink->Release(); event_sink = nullptr;
    if (!check(CoCreateInstance(CLSID_TF_ThreadMgr, nullptr, CLSCTX_INPROC_SERVER,
                  IID_ITfThreadMgr, reinterpret_cast<void **>(&manager)),
               "CoCreateInstance ThreadMgr")) break;
    if (!check(manager->Activate(&client), "ThreadMgr Activate")) break;
    manager_active = true;
    if (!check(service->Activate(manager, client), "TIP Activate")) break;
    service_active = true;
    if (!check(manager->CreateDocumentMgr(&doc), "CreateDocumentMgr")) break;
    store = CreateFixtureTextStore();
    TfEditCookie owner_cookie = 0;
    if (!check(doc->CreateContext(client, 0, store, &context, &owner_cookie),
               "CreateContext ACP")) break;
    if (!check(doc->Push(context), "Push context")) break;
    pushed = true;
    if (!check(manager->SetFocus(doc), "SetFocus")) break;
    if (preserved) {
      if (!check(manager->QueryInterface(IID_ITfKeystrokeMgr,
                    reinterpret_cast<void **>(&keys)), "QI KeystrokeMgr")) break;
      BOOL eaten = TRUE;
      HRESULT simulated = keys->SimulatePreservedKey(
          context, GUID_VoiceTypeCtrlCapsProbe, &eaten);
      LONG callbacks = 0;
      ULONGLONG serial = 0;
      BOOL nonforeground = FALSE;
      if (simulated != S_OK || eaten ||
          !check(probe->KeySnapshot(&callbacks, &serial, &nonforeground),
                 "KeySnapshot") || callbacks != 1 || serial == 0 || !nonforeground) {
        std::fprintf(stderr,
            "preserved-key mismatch: simulate=0x%08lx eaten=%d callbacks=%ld serial=%llu nonforeground=%d\n",
            static_cast<unsigned long>(simulated), eaten, callbacks, serial,
            nonforeground);
        break;
      }
      std::puts("PASS: nonforeground preserved key received current Msctf context");
      exit_code = 0;
      break;
    }
    const WCHAR text[] = L"你好 AI \U0001F600";
    if (!check(probe->Commit(context, text, static_cast<LONG>(wcslen(text))),
               "async request")) break;
    HRESULT edit = E_PENDING;
    LONG attempts = 0;
    DWORD deadline = GetTickCount() + 5000;
    while (GetTickCount() < deadline) {
      MSG message{};
      while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
        TranslateMessage(&message); DispatchMessageW(&message);
      }
      probe->Result(&edit, &attempts);
      if (edit != E_PENDING) break;
      MsgWaitForMultipleObjectsEx(0, nullptr, 10, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
    }
    if (!check(edit, "DoEditSession") || attempts != 1 ||
        FixtureMutationCount(store) != 1 || wcscmp(FixtureText(store), text) != 0) {
      std::fprintf(stderr, "SetText mismatch: attempts=%ld mutations=%ld edit=0x%08lx\n",
                   attempts, FixtureMutationCount(store), static_cast<unsigned long>(edit));
      break;
    }
    std::puts("PASS: real Msctf edit session wrote one UTF-16 result to ACP store");
    exit_code = 0;
  } while (false);
  if (service_active && service->Deactivate() != S_OK) {
    std::fputs("service deactivation or preserved-key cleanup failed\n", stderr);
    exit_code = 1;
  }
  if (preserved && keys && context && probe) {
    BOOL eaten = TRUE;
    HRESULT after = keys->SimulatePreservedKey(context,
        GUID_VoiceTypeCtrlCapsProbe, &eaten);
    LONG callbacks = 0;
    ULONGLONG serial = 0;
    BOOL nonforeground = TRUE;
    HRESULT snapshot = probe->KeySnapshot(&callbacks, &serial, &nonforeground);
    if (after != S_FALSE || snapshot != S_OK ||
        callbacks != 1 || nonforeground) {
      std::fprintf(stderr,
          "preserved key remained after deactivation: simulate=0x%08lx eaten=%d callbacks=%ld active=%d\n",
          static_cast<unsigned long>(after), eaten, callbacks, nonforeground);
      exit_code = 1;
    }
  }
  if (manager) manager->SetFocus(nullptr);
  if (pushed) doc->Pop(TF_POPF_ALL);
  if (context) context->Release();
  if (doc) doc->Release();
  if (store) store->Release();
  if (manager_active) manager->Deactivate();
  if (keys) keys->Release();
  if (manager) manager->Release();
  if (other) other->Release();
  if (canonical) canonical->Release();
  if (event_sink) event_sink->Release();
  if (service_ex) service_ex->Release();
  if (probe) probe->Release();
  if (service) service->Release();
  if (server_locked) {
    IClassFactory *unlock = factory;
    if (!unlock && get_factory) {
      get_factory(CLSID_VoiceTypeSpeechProbe, IID_IClassFactory,
                  reinterpret_cast<void **>(&unlock));
    }
    if (unlock) {
      unlock->LockServer(FALSE);
      if (unlock != factory) unlock->Release();
    }
  }
  if (factory) factory->Release();
  if (!can_unload || can_unload() != S_OK) {
    std::fputs("DLL remains referenced after sink and object cleanup\n", stderr);
    exit_code = 1;
  } else {
    FreeLibrary(module);
  }
  CoUninitialize();
  return exit_code;
}
