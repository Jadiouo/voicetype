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

`tsf_native_acp_once` loads the DLL directly, creates its COM factory, uses the
private `ActivateAOnly` method to test edit-session plumbing without a key
sink, and creates an
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
probe's own HKCU COM key. `tsf_registrar_rollback_policy` injects residual
profile/category/active states and failed observations into the same deletion
gate used by the registrar; it does not mutate the registry. The real-Msctf
`tsf_framework_activation_fixture` creates only this probe's fresh HKCU COM
CLSID, uses public TSF API to register a process-local speech profile, then
calls `ActivateProfile(FORPROCESS | ENABLEPROFILE)`. It never invokes the TIP's
`Activate` method itself. It requires an actual DLL `ACTIVATE`/`HELLO`, a
preserved-key callback in the current ACP context, `pfEaten=FALSE`, explicit
`DeactivateProfile(FORPROCESS | DISABLEPROFILE)`, and removal of its own
profile/category/COM key. The CI runner's account is disposable, but is not
proof of standard-user per-user registration, cross-process discovery, or
physical Ctrl+CapsLock in a desktop app. Any failure remains a red B gate;
the separate A-only test cannot make this one pass.

After A passes on MSVC, gate B must run under a real **non-elevated standard
user** in a disposable Windows 11 account. `voicetype_tsf_registrar` has
explicit `register`, `activate`, `deactivate`, `status`, and `unregister` actions; the
mutating actions refuse an elevated token. The DLL path must be absolute, and
the language is `0404` or `0409`. Capture command exit codes and printed
HRESULTs, actual service activation from a newly launched x64 target, preserved
Ctrl+CapsLock callback, and the keyboard profile before/after. `S_OK` from
`RegisterProfile` or `ActivateProfile` is **not** a load or coexistence pass.
Mutating actions require a successful original keyboard-profile query;
`ActivateProfile` returning `S_FALSE` is failure. HKCU COM registration rolls
back a newly created own key on write failure, and unregister refuses to
delete the key when it contains values or subkeys beyond this probe's schema.
After an activation attempt, explicitly request `DeactivateProfile` for the
desktop session. `unregister` observes its own profile, speech category,
active state and exact HKCU COM schema; it removes the COM key only after
confirmed TSF cleanup. Failed API calls or unknown state leave the key for
inspection and retry with a nonzero result. Stop if registration, cleanup,
or the original keyboard/IME check fails.

The bounded new-process B experiment is prepared as
`desktop/windows-tsf/Invoke-TsfBProbe.ps1 -Language 0409` (or `0404` for the
existing active language), after the build above. Run it only in a disposable,
interactive Windows 11 **standard-user** account. It registers and activates
the speech profile through public TSF APIs, launches a new x64 ACP target that
never loads the DLL directly, waits for diagnostic `ACTIVATE`/`HELLO`, simulates
the preserved key into that target's Msctf context, then deactivates and
unregisters in `finally`, verifies its own profile/category/COM state and a
fresh no-load target, and compares the keyboard identity. Metadata stays under
`.scratch/tsf-b/`: event, PID/TID, service instance, opaque context serial,
focus epoch and HRESULT, with no transcript, key text or pointer value. The
DLL uses `pfEaten=FALSE` in this diagnostic experiment; it neither records
audio nor changes the existing keyboard or IME. A failed registration, missing
new-process HELLO, missing callback, changed keyboard, or failed cleanup stops
B and requires inspection of the recorded HRESULT/status before any expansion.
Physical Ctrl+CapsLock in Notepad/Edge and profile behavior after reopening
remain separate Windows 11 manual gates; no synthetic CI result satisfies them.

The B callback proves its supplied context equals the **current** Msctf
focus/top at that instant. It does not create a lease valid across recording
or prove safe delivery after focus away-and-back. Current limits: the
diagnostic preserved-key sink is not connected to a broker
or recording. The DLL has no full focus/edit epochs,
selection-range lease, target IPC, production cancellation, password gate, or
auto-delivery. Its private `IVoiceTypeTsfProbe` is test-only. These are later
features only after B is supported by real Windows evidence.
