// New-process B gate. Never loads the probe DLL or changes TSF registration.
// Run only after the standard-user registrar has activated the speech profile.
#include "probe_api.h"
#include <textstor.h>
#include <windows.h>
#include <cstdio>
#include <cwchar>
#include <string>

extern "C" ITextStoreACP *CreateFixtureTextStore();

namespace {
template <class T> void release(T *&value) {
  if (value) { value->Release(); value = nullptr; }
}
bool same_keyboard(const TF_INPUTPROCESSORPROFILE &a,
                   const TF_INPUTPROCESSORPROFILE &b) {
  return a.dwProfileType == b.dwProfileType && a.langid == b.langid &&
         a.clsid == b.clsid && a.guidProfile == b.guidProfile && a.hkl == b.hkl;
}
bool saw_event(const WCHAR *path, const char *event, DWORD pid) {
  HANDLE file = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
                            nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
  if (file == INVALID_HANDLE_VALUE) return false;
  LARGE_INTEGER size{};
  bool found = false;
  if (GetFileSizeEx(file, &size) && size.QuadPart >= 0 && size.QuadPart <= 64 * 1024) {
    std::string bytes(static_cast<size_t>(size.QuadPart), '\0');
    DWORD count = 0;
    if (ReadFile(file, bytes.data(), static_cast<DWORD>(bytes.size()), &count,
                 nullptr)) {
      bytes.resize(count);
      const std::string marker = std::string("event=") + event + " pid=" +
                                 std::to_string(pid) + " ";
      found = bytes.find(marker) != std::string::npos;
    }
  }
  CloseHandle(file);
  return found;
}
bool wait_event(const WCHAR *path, const char *event, DWORD pid) {
  const ULONGLONG until = GetTickCount64() + 5000;
  while (GetTickCount64() < until) {
    if (saw_event(path, event, pid)) return true;
    MSG message{};
    while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
      TranslateMessage(&message);
      DispatchMessageW(&message);
    }
    MsgWaitForMultipleObjectsEx(0, nullptr, 10, QS_ALLINPUT,
                                MWMO_INPUTAVAILABLE);
  }
  return saw_event(path, event, pid);
}
}

int wmain(int argc, wchar_t **argv) {
  const bool expect_absent = argc == 3 && wcscmp(argv[2], L"--expect-absent") == 0;
  if ((argc != 2 && !expect_absent) || wcslen(argv[1]) < 3 || argv[1][1] != L':' ||
      argv[1][2] != L'\\') {
    std::fputs("usage: voicetype_tsf_b_target <new-absolute-log-path> [--expect-absent]\n", stderr);
    return 2;
  }
  HANDLE log = CreateFileW(argv[1], GENERIC_WRITE,
                           FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                           CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
  if (log == INVALID_HANDLE_VALUE) return 2;
  CloseHandle(log);
  if (!SetEnvironmentVariableW(L"VOICETYPE_TSF_B_LOG", argv[1])) return 2;
  HRESULT hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  if (hr != S_OK && hr != S_FALSE) return 1;
  int code = 1;
  ITfInputProcessorProfileMgr *profiles = nullptr;
  ITfThreadMgr *manager = nullptr;
  ITfDocumentMgr *doc = nullptr;
  ITfContext *context = nullptr;
  ITfKeystrokeMgr *keys = nullptr;
  ITextStoreACP *store = nullptr;
  TF_INPUTPROCESSORPROFILE before{}, after{};
  bool activated = false, pushed = false;
  TfClientId client = TF_CLIENTID_NULL;
  do {
    hr = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr,
        CLSCTX_INPROC_SERVER, IID_ITfInputProcessorProfileMgr,
        reinterpret_cast<void **>(&profiles));
    if (hr != S_OK || !profiles) break;
    if (profiles->GetActiveProfile(GUID_TFCAT_TIP_KEYBOARD, &before) != S_OK)
      break;
    hr = CoCreateInstance(CLSID_TF_ThreadMgr, nullptr, CLSCTX_INPROC_SERVER,
                          IID_ITfThreadMgr, reinterpret_cast<void **>(&manager));
    if (hr != S_OK || !manager) break;
    if (manager->Activate(&client) != S_OK) break;
    activated = true;
    if (manager->CreateDocumentMgr(&doc) != S_OK) break;
    store = CreateFixtureTextStore();
    if (!store) break;
    TfEditCookie owner_cookie = 0;
    if (doc->CreateContext(client, 0, store, &context, &owner_cookie) != S_OK)
      break;
    if (doc->Push(context) != S_OK) break;
    pushed = true;
    if (manager->SetFocus(doc) != S_OK) break;
    const DWORD pid = GetCurrentProcessId();
    if (expect_absent) {
      if (wait_event(argv[1], "ACTIVATE", pid) ||
          wait_event(argv[1], "HELLO", pid)) {
        std::fputs("speech TIP still auto-loaded after cleanup\n", stderr);
        break;
      }
      if (profiles->GetActiveProfile(GUID_TFCAT_TIP_KEYBOARD, &after) != S_OK ||
          !same_keyboard(before, after)) {
        std::fputs("keyboard profile changed after cleanup\n", stderr);
        break;
      }
      std::puts("PASS: new target did not load speech TIP after cleanup");
      code = 0;
      break;
    }
    if (!wait_event(argv[1], "ACTIVATE", pid) ||
        !wait_event(argv[1], "HELLO", pid)) {
      std::fputs("speech TIP did not auto-load and announce readiness in new process\n", stderr);
      break;
    }
    if (manager->QueryInterface(IID_ITfKeystrokeMgr,
          reinterpret_cast<void **>(&keys)) != S_OK) break;
    BOOL eaten = TRUE;
    hr = keys->SimulatePreservedKey(context, GUID_VoiceTypeCtrlCapsProbe,
                                    &eaten);
    if (hr != S_OK || eaten || !wait_event(argv[1], "PRESERVED", pid)) {
      std::fprintf(stderr,
          "preserved Ctrl+CapsLock context callback absent: hr=0x%08lx eaten=%d\n",
          static_cast<unsigned long>(hr), eaten);
      break;
    }
    if (profiles->GetActiveProfile(GUID_TFCAT_TIP_KEYBOARD, &after) != S_OK ||
        !same_keyboard(before, after)) {
      std::fputs("keyboard profile changed in speech target\n", stderr);
      break;
    }
    std::printf("PASS: speech TIP auto-loaded in pid=%lu; preserved context observed; keyboard unchanged\n",
                pid);
    code = 0;
  } while (false);
  if (manager) manager->SetFocus(nullptr);
  if (pushed) doc->Pop(TF_POPF_ALL);
  release(keys);
  release(context);
  release(doc);
  release(store);
  if (activated) manager->Deactivate();
  release(manager);
  release(profiles);
  CoUninitialize();
  return code;
}
