# Isolated Windows TSF speech probe

This directory is an experimental, x64 MSVC-only gate for APP-03. It is not
bundled by the VoiceType installer or activated by the desktop app. The existing
Windows worker still retains recognized text and makes no automatic insertion.

From the repository root on a Windows 11 x64 development machine:

```powershell
$env:TEMP = Join-Path $PWD '.scratch/tsf-temp'
$env:TMP = $env:TEMP
New-Item -ItemType Directory -Force -Path $env:TEMP | Out-Null
cmake -S desktop/windows-tsf -B .scratch/tsf-native/build -A x64
cmake --build .scratch/tsf-native/build --config Release --parallel 2
ctest --test-dir .scratch/tsf-native/build -C Release --output-on-failure
```

`tsf_native_acp_once` loads the DLL directly, creates its COM factory, activates
the service manually on a real `Msctf.dll` thread manager, creates an
`ITextStoreACP` context, and requests one asynchronous edit session. It checks
the exact UTF-16 text, one text mutation, deactivation, sink release, and
`DllCanUnloadNow`. It also checks canonical `IUnknown` identity and balanced
COM object/server references. A failed or still-pending edit keeps the DLL
loaded until process exit if COM objects remain; the harness never calls
`FreeLibrary` on a referenced DLL. This is a harness-controlled TSF operation, not proof that
Windows will discover the speech profile in another application.

`tsf_broker_protocol` is an in-process fixed-width frame and state-transition
probe. It proves no named-pipe ACL, identity binding, cross-process CAS, or
delivery ACK yet. `tsf_registrar_status_readonly` only reads TSF state and the
probe's own HKCU COM key; CI never registers a profile.

After A passes on MSVC, gate B must run under a real **non-elevated standard
user** in a disposable Windows 11 account. `voicetype_tsf_registrar` has
explicit `register`, `activate`, `status`, and `unregister` actions; the
mutating actions refuse an elevated token. The DLL path must be absolute, and
the language is `0404` or `0409`. Capture command exit codes and printed
HRESULTs, actual service activation from a newly launched x64 target, preserved
Ctrl+CapsLock callback, and the keyboard profile before/after. `S_OK` from
`RegisterProfile` or `ActivateProfile` is **not** a load or coexistence pass.
Mutating actions require a successful original keyboard-profile query;
`ActivateProfile` returning `S_FALSE` is failure. HKCU COM registration rolls
back a newly created own key on write failure, and unregister refuses to
delete the key when it contains values or subkeys beyond this probe's schema.
Run `unregister` in a `finally`/cleanup step. Stop the experiment if standard
user registration fails or the existing keyboard/IME profile changes.

Current limits: the DLL has no preserved-key sink, full focus/edit epochs,
selection-range lease, target IPC, production cancellation, password gate, or
auto-delivery. Its private `IVoiceTypeTsfProbe` is test-only. These are the next
features only after A and B are supported by real Windows evidence.
