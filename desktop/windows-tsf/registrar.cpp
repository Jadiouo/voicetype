// Explicit standard-user TSF registration probe. Not called by the app/installer.
#include "probe_api.h"
#include <windows.h>
#include <msctf.h>
#include <cstdio>
#include <cwchar>
#include <string>

namespace {
template<class T> void release(T *&p) { if (p) { p->Release(); p = nullptr; } }
void report(const char *step, HRESULT hr) {
  std::fprintf(stderr, "%s: 0x%08lx\n", step, static_cast<unsigned long>(hr));
}
bool standard_token() {
  HANDLE token = nullptr;
  if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) return false;
  TOKEN_ELEVATION elevation{};
  DWORD size = 0;
  bool result = GetTokenInformation(token, TokenElevation, &elevation,
                                    sizeof(elevation), &size) && !elevation.TokenIsElevated;
  CloseHandle(token);
  return result;
}
std::wstring com_key() {
  WCHAR id[40]{};
  StringFromGUID2(CLSID_VoiceTypeSpeechProbe, id, 40);
  return std::wstring(L"Software\\Classes\\CLSID\\") + id;
}
std::wstring registered_path() {
  HKEY key = nullptr;
  auto path = com_key() + L"\\InprocServer32";
  if (RegOpenKeyExW(HKEY_CURRENT_USER, path.c_str(), 0, KEY_READ, &key) != ERROR_SUCCESS)
    return {};
  WCHAR value[32768]{};
  DWORD kind = 0, bytes = sizeof(value);
  LONG status = RegQueryValueExW(key, nullptr, nullptr, &kind,
                                 reinterpret_cast<BYTE *>(value), &bytes);
  RegCloseKey(key);
  if (status != ERROR_SUCCESS || kind != REG_SZ || bytes < sizeof(WCHAR) ||
      value[bytes / sizeof(WCHAR) - 1] != 0) return {};
  return value;
}
HRESULT put_com_path(const std::wstring &dll) {
  const auto root_path = com_key();
  HKEY existing = nullptr;
  LONG status = RegOpenKeyExW(HKEY_CURRENT_USER, root_path.c_str(), 0,
                              KEY_READ, &existing);
  if (status == ERROR_SUCCESS) {
    RegCloseKey(existing);
    return HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS);
  }
  if (status != ERROR_FILE_NOT_FOUND) return HRESULT_FROM_WIN32(status);
  HKEY root = nullptr, key = nullptr;
  DWORD disposition = 0;
  status = RegCreateKeyExW(HKEY_CURRENT_USER, root_path.c_str(), 0, nullptr,
                            REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE,
                            nullptr, &root, &disposition);
  if (status != ERROR_SUCCESS) return HRESULT_FROM_WIN32(status);
  if (disposition != REG_CREATED_NEW_KEY) {
    RegCloseKey(root);
    return HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS);
  }
  DWORD child_disposition = 0;
  status = RegCreateKeyExW(root, L"InprocServer32", 0, nullptr,
                            REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE,
                            nullptr, &key, &child_disposition);
  if (status != ERROR_SUCCESS) {
    RegCloseKey(root);
    LONG rollback = RegDeleteTreeW(HKEY_CURRENT_USER, root_path.c_str());
    if (rollback != ERROR_SUCCESS) return HRESULT_FROM_WIN32(rollback);
    return HRESULT_FROM_WIN32(status);
  }
  if (child_disposition != REG_CREATED_NEW_KEY) {
    RegCloseKey(key);
    RegCloseKey(root);
    return HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS);
  }
  status = RegSetValueExW(key, nullptr, 0, REG_SZ,
      reinterpret_cast<const BYTE *>(dll.c_str()),
      static_cast<DWORD>((dll.size() + 1) * sizeof(WCHAR)));
  const WCHAR model[] = L"Apartment";
  if (status == ERROR_SUCCESS)
    status = RegSetValueExW(key, L"ThreadingModel", 0, REG_SZ,
          reinterpret_cast<const BYTE *>(model), sizeof(model));
  RegCloseKey(key);
  RegCloseKey(root);
  if (status != ERROR_SUCCESS) {
    LONG rollback = RegDeleteTreeW(HKEY_CURRENT_USER, root_path.c_str());
    if (rollback != ERROR_SUCCESS) return HRESULT_FROM_WIN32(rollback);
  }
  return HRESULT_FROM_WIN32(status);
}
bool own_com_path_matches(const std::wstring &dll) {
  HKEY root = nullptr, key = nullptr;
  if (RegOpenKeyExW(HKEY_CURRENT_USER, com_key().c_str(), 0, KEY_READ,
                    &root) != ERROR_SUCCESS) return false;
  DWORD subkeys = 0, values = 0;
  LONG status = RegQueryInfoKeyW(root, nullptr, nullptr, nullptr, &subkeys,
                                  nullptr, nullptr, &values, nullptr, nullptr,
                                  nullptr, nullptr);
  WCHAR child[32]{};
  DWORD child_length = 32;
  bool exact = status == ERROR_SUCCESS && subkeys == 1 && values == 0 &&
      RegEnumKeyExW(root, 0, child, &child_length, nullptr, nullptr, nullptr,
                    nullptr) == ERROR_SUCCESS &&
      _wcsicmp(child, L"InprocServer32") == 0;
  if (exact) exact = RegOpenKeyExW(root, L"InprocServer32", 0, KEY_READ,
                                   &key) == ERROR_SUCCESS;
  RegCloseKey(root);
  if (!exact) return false;
  status = RegQueryInfoKeyW(key, nullptr, nullptr, nullptr, &subkeys,
                            nullptr, nullptr, &values, nullptr, nullptr,
                            nullptr, nullptr);
  bool default_value = false, model_value = false;
  exact = status == ERROR_SUCCESS && subkeys == 0 && values == 2;
  for (DWORD i = 0; exact && i < values; ++i) {
    WCHAR name[64]{};
    DWORD length = 64;
    exact = RegEnumValueW(key, i, name, &length, nullptr, nullptr, nullptr,
                          nullptr) == ERROR_SUCCESS;
    if (!exact) break;
    if (length == 0) default_value = true;
    else if (_wcsicmp(name, L"ThreadingModel") == 0) model_value = true;
    else exact = false;
  }
  WCHAR model[32]{};
  DWORD bytes = sizeof(model), kind = 0;
  if (exact) exact = default_value && model_value &&
      RegQueryValueExW(key, L"ThreadingModel", nullptr, &kind,
                       reinterpret_cast<BYTE *>(model), &bytes) == ERROR_SUCCESS &&
      kind == REG_SZ && bytes == sizeof(L"Apartment") &&
      wcscmp(model, L"Apartment") == 0;
  RegCloseKey(key);
  return exact && registered_path() == dll;
}
HRESULT remove_com_path(const std::wstring &dll) {
  if (!own_com_path_matches(dll)) return HRESULT_FROM_WIN32(ERROR_INVALID_DATA);
  return HRESULT_FROM_WIN32(RegDeleteTreeW(HKEY_CURRENT_USER, com_key().c_str()));
}
HRESULT get_apis(ITfInputProcessorProfileMgr **manager,
                 ITfInputProcessorProfiles **profiles,
                 ITfCategoryMgr **categories) {
  HRESULT hr = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr,
      CLSCTX_INPROC_SERVER, IID_ITfInputProcessorProfileMgr,
      reinterpret_cast<void **>(manager));
  if (FAILED(hr)) return hr;
  hr = (*manager)->QueryInterface(IID_ITfInputProcessorProfiles,
                                  reinterpret_cast<void **>(profiles));
  if (FAILED(hr)) return hr;
  return CoCreateInstance(CLSID_TF_CategoryMgr, nullptr, CLSCTX_INPROC_SERVER,
      IID_ITfCategoryMgr, reinterpret_cast<void **>(categories));
}
bool same_keyboard(const TF_INPUTPROCESSORPROFILE &a,
                   const TF_INPUTPROCESSORPROFILE &b) {
  return a.dwProfileType == b.dwProfileType && a.langid == b.langid &&
         a.clsid == b.clsid && a.guidProfile == b.guidProfile && a.hkl == b.hkl;
}
}

int wmain(int argc, wchar_t **argv) {
  if (argc < 2 || argc > 4) {
    std::fputs("usage: registrar status | register|activate|unregister <absolute-dll> <0404|0409>\n", stderr);
    return 2;
  }
  const std::wstring action = argv[1];
  if (action != L"status" && action != L"register" && action != L"activate" &&
      action != L"unregister") return 2;
  if (action == L"status" && argc != 2) return 2;
  if (action != L"status" && argc != 4) return 2;
  LANGID lang = 0;
  std::wstring path;
  if (action != L"status") {
    if (!standard_token()) {
      std::fputs("refusing registration probe outside a non-elevated token\n", stderr);
      return 3;
    }
    if (wcscmp(argv[3], L"0404") == 0) lang = 0x0404;
    else if (wcscmp(argv[3], L"0409") == 0) lang = 0x0409;
    else return 2;
    path = argv[2];
    if (path.size() < 3 || path[1] != L':' || path[2] != L'\\' ||
        GetFileAttributesW(path.c_str()) == INVALID_FILE_ATTRIBUTES) return 2;
  }
  HRESULT hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  if (FAILED(hr)) { report("CoInitializeEx", hr); return 1; }
  ITfInputProcessorProfileMgr *manager = nullptr;
  ITfInputProcessorProfiles *profiles = nullptr;
  ITfCategoryMgr *categories = nullptr;
  hr = get_apis(&manager, &profiles, &categories);
  if (FAILED(hr)) { report("TSF APIs", hr); release(categories); release(profiles);
                    release(manager); CoUninitialize(); return 1; }
  int code = 0;
  TF_INPUTPROCESSORPROFILE keyboard_before{}, keyboard_after{};
  HRESULT keyboard_hr = manager->GetActiveProfile(GUID_TFCAT_TIP_KEYBOARD,
                                                   &keyboard_before);
  if (action != L"status" && keyboard_hr != S_OK) {
    report("cannot establish original keyboard profile", keyboard_hr);
    release(categories); release(profiles); release(manager);
    CoUninitialize(); return 5;
  }
  LANGID current = 0;
  hr = profiles->GetCurrentLanguage(&current);
  if (action == L"status") {
    LANGID active_lang = 0;
    GUID active_guid{};
    HRESULT active = profiles->GetActiveLanguageProfile(
        CLSID_VoiceTypeSpeechProbe, &active_lang, &active_guid);
    std::printf("current_language=0x%04x hr=0x%08lx own_active_hr=0x%08lx keyboard_hr=0x%08lx registered_hkcu=%d\n",
        current, static_cast<unsigned long>(hr), static_cast<unsigned long>(active),
        static_cast<unsigned long>(keyboard_hr), !registered_path().empty());
  } else if (action == L"register") {
    if (own_com_path_matches(path) || !registered_path().empty()) { code = 4; }
    else {
      hr = put_com_path(path); report("HKCU COM", hr);
      if (hr == S_OK) {
        const WCHAR desc[] = L"VoiceType Speech Probe";
        hr = manager->RegisterProfile(CLSID_VoiceTypeSpeechProbe, lang,
            GUID_VoiceTypeSpeechProfile, desc, static_cast<ULONG>(wcslen(desc)),
            path.c_str(), static_cast<ULONG>(path.size()), 0, nullptr, 0, FALSE, 0);
        report("RegisterProfile speech", hr);
        if (hr == S_OK) {
          hr = categories->RegisterCategory(CLSID_VoiceTypeSpeechProbe,
              GUID_TFCAT_TIP_SPEECH, CLSID_VoiceTypeSpeechProbe);
          report("RegisterCategory speech", hr);
        }
        if (hr == S_OK) {
          hr = profiles->EnableLanguageProfile(CLSID_VoiceTypeSpeechProbe, lang,
                                                GUID_VoiceTypeSpeechProfile, TRUE);
          report("EnableLanguageProfile current user", hr);
        }
        if (hr != S_OK) {
          categories->UnregisterCategory(CLSID_VoiceTypeSpeechProbe,
              GUID_TFCAT_TIP_SPEECH, CLSID_VoiceTypeSpeechProbe);
          manager->UnregisterProfile(CLSID_VoiceTypeSpeechProbe, lang,
                                     GUID_VoiceTypeSpeechProfile, 0);
          HRESULT rollback = remove_com_path(path);
          if (rollback != S_OK) report("HKCU COM rollback", rollback);
          code = 1;
        }
      } else code = 1;
    }
  } else if (action == L"activate") {
    if (!own_com_path_matches(path)) code = 4;
    else {
      hr = manager->ActivateProfile(TF_PROFILETYPE_INPUTPROCESSOR, lang,
          CLSID_VoiceTypeSpeechProbe, GUID_VoiceTypeSpeechProfile, nullptr,
          TF_IPPMF_FORSESSION);
      report("ActivateProfile request (not load proof)", hr);
      if (hr != S_OK) code = 1;
    }
  } else {
    if (!own_com_path_matches(path)) code = 4;
    else {
      HRESULT disable = profiles->EnableLanguageProfile(CLSID_VoiceTypeSpeechProbe,
                                 lang, GUID_VoiceTypeSpeechProfile, FALSE);
      HRESULT profile = manager->UnregisterProfile(CLSID_VoiceTypeSpeechProbe, lang,
                                 GUID_VoiceTypeSpeechProfile, 0);
      HRESULT category = categories->UnregisterCategory(CLSID_VoiceTypeSpeechProbe,
                                 GUID_TFCAT_TIP_SPEECH, CLSID_VoiceTypeSpeechProbe);
      report("disable/profile/category", disable);
      report("UnregisterProfile", profile); report("UnregisterCategory", category);
      if (disable == S_OK && profile == S_OK && category == S_OK) {
        HRESULT removed = remove_com_path(path);
        if (removed != S_OK) { report("HKCU COM removal", removed); code = 1; }
      } else code = 1;
    }
  }
  if (action != L"status") {
    HRESULT after = manager->GetActiveProfile(GUID_TFCAT_TIP_KEYBOARD,
                                               &keyboard_after);
    if (after != S_OK || !same_keyboard(keyboard_before, keyboard_after)) {
      std::fputs("keyboard profile changed during speech probe\n", stderr);
      HRESULT restore = manager->ActivateProfile(keyboard_before.dwProfileType,
          keyboard_before.langid, keyboard_before.clsid,
          keyboard_before.guidProfile, keyboard_before.hkl, TF_IPPMF_FORSESSION);
      report("restore original keyboard profile", restore);
      code = 5;
    }
  }
  release(categories); release(profiles); release(manager);
  CoUninitialize();
  return code;
}
