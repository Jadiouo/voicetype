// CI-only experiment: public process-local TSF profile, genuine TSF activation.
// It never calls ITfTextInputProcessor::Activate itself.
#include "probe_api.h"
#include <windows.h>
#include <msctf.h>
#include <textstor.h>
#include <cstdio>
#include <cwchar>
#include <string>

extern "C" ITextStoreACP *CreateFixtureTextStore();

namespace {
template<class T> void release(T *&value) {
  if (value) { value->Release(); value = nullptr; }
}
void report(const char *step, HRESULT hr) {
  std::fprintf(stderr, "%s: 0x%08lx\n", step, static_cast<unsigned long>(hr));
}
std::wstring own_key() {
  WCHAR id[40]{};
  StringFromGUID2(CLSID_VoiceTypeSpeechProbe, id, 40);
  return std::wstring(L"Software\\Classes\\CLSID\\") + id;
}
bool exact_com_key(const std::wstring &dll) {
  HKEY root = nullptr, child = nullptr;
  if (RegOpenKeyExW(HKEY_CURRENT_USER, own_key().c_str(), 0,
                    KEY_READ, &root) != ERROR_SUCCESS) return false;
  DWORD subkeys = 0, values = 0;
  LONG status = RegQueryInfoKeyW(root, nullptr, nullptr, nullptr, &subkeys,
                                  nullptr, nullptr, &values, nullptr, nullptr,
                                  nullptr, nullptr);
  WCHAR subkey[32]{};
  DWORD length = 32;
  bool exact = status == ERROR_SUCCESS && subkeys == 1 && values == 0 &&
      RegEnumKeyExW(root, 0, subkey, &length, nullptr, nullptr, nullptr,
                    nullptr) == ERROR_SUCCESS &&
      _wcsicmp(subkey, L"InprocServer32") == 0 &&
      RegOpenKeyExW(root, L"InprocServer32", 0, KEY_READ, &child) == ERROR_SUCCESS;
  RegCloseKey(root);
  if (!exact) return false;
  status = RegQueryInfoKeyW(child, nullptr, nullptr, nullptr, &subkeys,
                            nullptr, nullptr, &values, nullptr, nullptr,
                            nullptr, nullptr);
  exact = status == ERROR_SUCCESS && subkeys == 0 && values == 2;
  WCHAR path[32768]{}, model[32]{};
  DWORD path_bytes = sizeof(path), model_bytes = sizeof(model);
  DWORD path_type = 0, model_type = 0;
  if (exact) exact =
      RegQueryValueExW(child, nullptr, nullptr, &path_type,
          reinterpret_cast<BYTE *>(path), &path_bytes) == ERROR_SUCCESS &&
      RegQueryValueExW(child, L"ThreadingModel", nullptr, &model_type,
          reinterpret_cast<BYTE *>(model), &model_bytes) == ERROR_SUCCESS &&
      path_type == REG_SZ && model_type == REG_SZ &&
      path_bytes == (dll.size() + 1) * sizeof(WCHAR) &&
      model_bytes == sizeof(L"Apartment") && path == dll &&
      wcscmp(model, L"Apartment") == 0;
  RegCloseKey(child);
  return exact;
}
bool create_own_com(const std::wstring &dll) {
  HKEY existing = nullptr;
  LONG status = RegOpenKeyExW(HKEY_CURRENT_USER, own_key().c_str(), 0,
                              KEY_READ, &existing);
  if (status == ERROR_SUCCESS) { RegCloseKey(existing); return false; }
  if (status != ERROR_FILE_NOT_FOUND) return false;
  HKEY root = nullptr, child = nullptr;
  DWORD disposition = 0;
  status = RegCreateKeyExW(HKEY_CURRENT_USER, own_key().c_str(), 0, nullptr,
      REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE, nullptr, &root,
      &disposition);
  if (status != ERROR_SUCCESS) return false;
  if (disposition != REG_CREATED_NEW_KEY) { RegCloseKey(root); return false; }
  status = RegCreateKeyExW(root, L"InprocServer32", 0, nullptr,
      REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE, nullptr, &child,
      &disposition);
  if (status == ERROR_SUCCESS && disposition == REG_CREATED_NEW_KEY) {
    status = RegSetValueExW(child, nullptr, 0, REG_SZ,
        reinterpret_cast<const BYTE *>(dll.c_str()),
        static_cast<DWORD>((dll.size() + 1) * sizeof(WCHAR)));
    const WCHAR model[] = L"Apartment";
    if (status == ERROR_SUCCESS)
      status = RegSetValueExW(child, L"ThreadingModel", 0, REG_SZ,
          reinterpret_cast<const BYTE *>(model), sizeof(model));
  }
  if (child) RegCloseKey(child);
  RegCloseKey(root);
  if (status != ERROR_SUCCESS || disposition != REG_CREATED_NEW_KEY) {
    std::fputs("owned COM creation incomplete; inspect runner's own CLSID\n", stderr);
    return false;
  }
  return exact_com_key(dll);
}
bool remove_own_com(const std::wstring &dll) {
  if (!exact_com_key(dll)) return false;
  return RegDeleteTreeW(HKEY_CURRENT_USER, own_key().c_str()) == ERROR_SUCCESS;
}
bool com_absent() {
  HKEY key = nullptr;
  LONG status = RegOpenKeyExW(HKEY_CURRENT_USER, own_key().c_str(), 0,
                              KEY_READ, &key);
  if (status == ERROR_SUCCESS) RegCloseKey(key);
  return status == ERROR_FILE_NOT_FOUND;
}
bool profile_absent(ITfInputProcessorProfileMgr *profiles, LANGID lang) {
  IEnumTfInputProcessorProfiles *items = nullptr;
  if (profiles->EnumProfiles(lang, &items) != S_OK || !items) return false;
  bool absent = true;
  for (;;) {
    TF_INPUTPROCESSORPROFILE item{};
    ULONG fetched = 0;
    HRESULT hr = items->Next(1, &item, &fetched);
    if (hr == S_FALSE && fetched == 0) break;
    if (hr != S_OK || fetched != 1) { absent = false; break; }
    if (item.clsid == CLSID_VoiceTypeSpeechProbe &&
        item.guidProfile == GUID_VoiceTypeSpeechProfile) {
      absent = false; break;
    }
  }
  items->Release();
  return absent;
}
bool category_absent(ITfCategoryMgr *categories) {
  IEnumGUID *items = nullptr;
  if (categories->EnumCategoriesInItem(CLSID_VoiceTypeSpeechProbe,
                                        &items) != S_OK || !items) return false;
  bool absent = true;
  for (;;) {
    GUID item{};
    ULONG fetched = 0;
    HRESULT hr = items->Next(1, &item, &fetched);
    if (hr == S_FALSE && fetched == 0) break;
    if (hr != S_OK || fetched != 1) { absent = false; break; }
    if (item == GUID_TFCAT_TIP_SPEECH) { absent = false; break; }
  }
  items->Release();
  return absent;
}
bool saw_event(const std::wstring &path, const char *event) {
  HANDLE file = CreateFileW(path.c_str(), GENERIC_READ,
      FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL, nullptr);
  if (file == INVALID_HANDLE_VALUE) return false;
  LARGE_INTEGER size{};
  bool found = false;
  if (GetFileSizeEx(file, &size) && size.QuadPart >= 0 &&
      size.QuadPart <= 64 * 1024) {
    std::string data(static_cast<size_t>(size.QuadPart), '\0');
    DWORD count = 0;
    if (ReadFile(file, data.data(), static_cast<DWORD>(data.size()),
                 &count, nullptr)) {
      data.resize(count);
      std::string marker = std::string("event=") + event + " pid=" +
                           std::to_string(GetCurrentProcessId()) + " ";
      found = data.find(marker) != std::string::npos;
    }
  }
  CloseHandle(file);
  return found;
}
bool wait_event(const std::wstring &path, const char *event) {
  ULONGLONG until = GetTickCount64() + 5000;
  while (GetTickCount64() < until) {
    if (saw_event(path, event)) return true;
    MSG message{};
    while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
      TranslateMessage(&message); DispatchMessageW(&message);
    }
    MsgWaitForMultipleObjectsEx(0, nullptr, 10, QS_ALLINPUT,
                                MWMO_INPUTAVAILABLE);
  }
  return saw_event(path, event);
}
}

int wmain(int argc, wchar_t **argv) {
  if (argc != 2) return 2;
  const std::wstring dll = argv[1];
  const std::wstring log_path = dll + L".framework-" +
      std::to_wstring(GetCurrentProcessId()) + L"-" +
      std::to_wstring(GetTickCount64()) + L".log";
  HANDLE log = CreateFileW(log_path.c_str(), GENERIC_WRITE,
      FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, CREATE_NEW,
      FILE_ATTRIBUTE_NORMAL, nullptr);
  if (log == INVALID_HANDLE_VALUE) return 2;
  CloseHandle(log);
  if (!SetEnvironmentVariableW(L"VOICETYPE_TSF_B_LOG", log_path.c_str())) return 2;
  HRESULT hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  if (hr != S_OK && hr != S_FALSE) return 2;
  int code = 1;
  bool com_owned = false, category_owned = false, profile_owned = false;
  bool manager_active = false, context_pushed = false, activation_requested = false;
  ITfInputProcessorProfileMgr *profiles = nullptr;
  ITfCategoryMgr *categories = nullptr;
  ITfThreadMgr *manager = nullptr;
  ITfDocumentMgr *doc = nullptr;
  ITfContext *context = nullptr;
  ITfKeystrokeMgr *keys = nullptr;
  ITextStoreACP *store = nullptr;
  TfClientId app_client = TF_CLIENTID_NULL;
  LANGID lang = 0;
  do {
    if (!com_absent()) { std::fputs("probe CLSID already exists\n", stderr); break; }
    if (!create_own_com(dll)) { std::fputs("own HKCU COM registration failed\n", stderr); break; }
    com_owned = true;
    hr = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr,
        CLSCTX_INPROC_SERVER, IID_ITfInputProcessorProfileMgr,
        reinterpret_cast<void **>(&profiles));
    if (hr != S_OK) { report("ProfileMgr", hr); break; }
    hr = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr,
        CLSCTX_INPROC_SERVER, IID_ITfCategoryMgr,
        reinterpret_cast<void **>(&categories));
    if (hr != S_OK) { report("CategoryMgr", hr); break; }
    ITfInputProcessorProfiles *languages = nullptr;
    hr = profiles->QueryInterface(IID_ITfInputProcessorProfiles,
                                  reinterpret_cast<void **>(&languages));
    if (hr == S_OK) hr = languages->GetCurrentLanguage(&lang);
    release(languages);
    if (hr != S_OK) { report("current language", hr); break; }
    if (!profile_absent(profiles, lang) || !category_absent(categories)) {
      std::fputs("probe TSF profile/category already exists\n", stderr);
      break;
    }
    category_owned = true; // Also attempt rollback if the API partially succeeds.
    hr = categories->RegisterCategory(CLSID_VoiceTypeSpeechProbe,
        GUID_TFCAT_TIP_SPEECH, CLSID_VoiceTypeSpeechProbe);
    if (hr != S_OK) { report("speech category", hr); break; }
    const WCHAR desc[] = L"VoiceType process-local speech fixture";
    profile_owned = true; // Public API may mutate before returning failure.
    hr = profiles->RegisterProfile(CLSID_VoiceTypeSpeechProbe, lang,
        GUID_VoiceTypeSpeechProfile, desc, static_cast<ULONG>(wcslen(desc)),
        dll.c_str(), static_cast<ULONG>(dll.size()), 0, nullptr, 0,
        TRUE, TF_RP_LOCALPROCESS);
    if (hr != S_OK) { report("LOCALPROCESS profile", hr); break; }
    hr = CoCreateInstance(CLSID_TF_ThreadMgr, nullptr, CLSCTX_INPROC_SERVER,
                          IID_ITfThreadMgr, reinterpret_cast<void **>(&manager));
    if (hr != S_OK) { report("ThreadMgr", hr); break; }
    hr = manager->Activate(&app_client);
    if (hr != S_OK) { report("ThreadMgr Activate", hr); break; }
    manager_active = true;
    hr = manager->CreateDocumentMgr(&doc);
    if (hr != S_OK) { report("CreateDocumentMgr", hr); break; }
    store = CreateFixtureTextStore();
    if (!store) break;
    TfEditCookie owner_cookie = 0;
    hr = doc->CreateContext(app_client, 0, store, &context, &owner_cookie);
    if (hr != S_OK) { report("CreateContext", hr); break; }
    hr = doc->Push(context);
    if (hr != S_OK) { report("Push", hr); break; }
    context_pushed = true;
    hr = manager->SetFocus(doc);
    if (hr != S_OK) { report("SetFocus", hr); break; }
    activation_requested = true;
    hr = profiles->ActivateProfile(TF_PROFILETYPE_INPUTPROCESSOR, lang,
        CLSID_VoiceTypeSpeechProbe, GUID_VoiceTypeSpeechProfile, nullptr,
        TF_IPPMF_FORPROCESS | TF_IPPMF_ENABLEPROFILE);
    report("ActivateProfile FORPROCESS+ENABLEPROFILE (not load proof)", hr);
    if (hr != S_OK) break;
    if (!wait_event(log_path, "ACTIVATE") || !wait_event(log_path, "HELLO")) {
      std::fputs("TSF did not call TIP Activate/HELLO\n", stderr); break;
    }
    hr = manager->QueryInterface(IID_ITfKeystrokeMgr,
                                  reinterpret_cast<void **>(&keys));
    if (hr != S_OK) { report("KeystrokeMgr", hr); break; }
    BOOL eaten = TRUE;
    hr = keys->SimulatePreservedKey(context, GUID_VoiceTypeCtrlCapsProbe,
                                    &eaten);
    report("SimulatePreservedKey", hr);
    if (hr != S_OK || eaten || !wait_event(log_path, "PRESERVED")) {
      std::fputs("TSF preserved-key callback missing\n", stderr); break;
    }
    std::puts("PASS: TSF activated speech TIP, HELLO and current-context callback observed");
    code = 0;
  } while (false);
  if (activation_requested && profiles) {
    hr = profiles->DeactivateProfile(TF_PROFILETYPE_INPUTPROCESSOR, lang,
        CLSID_VoiceTypeSpeechProbe, GUID_VoiceTypeSpeechProfile, nullptr,
        TF_IPPMF_FORPROCESS | TF_IPPMF_DISABLEPROFILE);
    report("DeactivateProfile FORPROCESS+DISABLEPROFILE", hr);
    if (hr != S_OK) code = 1;
    ITfInputProcessorProfiles *languages = nullptr;
    HRESULT query = profiles->QueryInterface(IID_ITfInputProcessorProfiles,
        reinterpret_cast<void **>(&languages));
    LANGID active_lang = 0;
    GUID active_guid{};
    if (query == S_OK)
      query = languages->GetActiveLanguageProfile(CLSID_VoiceTypeSpeechProbe,
                                                  &active_lang, &active_guid);
    report("GetActiveLanguageProfile after deactivation (expect S_FALSE)", query);
    if (query != S_FALSE) code = 1;
    release(languages);
  }
  if (manager) manager->SetFocus(nullptr);
  if (context_pushed && doc) doc->Pop(TF_POPF_ALL);
  release(keys); release(context); release(doc); release(store);
  if (manager_active && manager->Deactivate() != S_OK) code = 1;
  release(manager);
  if (profile_owned && profiles) {
    hr = profiles->UnregisterProfile(CLSID_VoiceTypeSpeechProbe, lang,
                                     GUID_VoiceTypeSpeechProfile,
                                     TF_URP_LOCALPROCESS);
    report("UnregisterProfile LOCALPROCESS", hr);
    const bool absent = profile_absent(profiles, lang);
    std::printf("post_cleanup profile_absent=%d\n", absent);
    if (hr != S_OK || !absent) code = 1;
  }
  if (category_owned && categories) {
    hr = categories->UnregisterCategory(CLSID_VoiceTypeSpeechProbe,
        GUID_TFCAT_TIP_SPEECH, CLSID_VoiceTypeSpeechProbe);
    report("UnregisterCategory speech", hr);
    const bool absent = category_absent(categories);
    std::printf("post_cleanup category_absent=%d\n", absent);
    if (hr != S_OK || !absent) code = 1;
  }
  release(categories); release(profiles);
  if (com_owned && !remove_own_com(dll)) {
    std::fputs("own HKCU COM cleanup failed or key changed\n", stderr);
    code = 1;
  }
  const bool com_clear = com_absent();
  std::printf("post_cleanup com_absent=%d\n", com_clear);
  if (!com_clear) code = 1;
  if (code == 0 &&
      (!saw_event(log_path, "UNPRESERVE") ||
       !saw_event(log_path, "UNADVISE_KEY") ||
       !saw_event(log_path, "DEACTIVATE"))) {
    std::fputs("key sink cleanup callbacks missing\n", stderr);
    code = 1;
  }
  if (code == 0) std::puts("PASS: profile/category/COM cleaned after TSF deactivation");
  CoUninitialize();
  return code;
}
