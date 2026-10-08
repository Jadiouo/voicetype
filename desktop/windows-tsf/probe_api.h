#pragma once

#include <msctf.h>

// Private, harness-only interface. Production activation and transcript IPC do
// not use this interface; no desktop package loads this experimental DLL.
struct __declspec(uuid("BD13FFCF-13E8-44B7-BB65-1B355D3F9DD1")) IVoiceTypeTsfProbe
    : IUnknown {
  virtual HRESULT STDMETHODCALLTYPE Commit(ITfContext *context,
                                           const WCHAR *text,
                                           LONG length) = 0;
  virtual HRESULT STDMETHODCALLTYPE Result(HRESULT *edit_result,
                                           LONG *attempts) = 0;
};

inline constexpr CLSID CLSID_VoiceTypeSpeechProbe = {
    0x819a65cd, 0x254b, 0x40e6,
    {0xa6, 0x43, 0x69, 0xd6, 0x30, 0x11, 0x58, 0x9e}};
inline constexpr GUID GUID_VoiceTypeSpeechProfile = {
    0x0be09a1b, 0x115f, 0x4c4f,
    {0x87, 0x56, 0xe9, 0x89, 0xfc, 0xa0, 0x25, 0xf4}};
