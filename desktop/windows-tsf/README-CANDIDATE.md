# VoiceType TSF B diagnostic candidate

This package tests whether Windows 11 can activate the isolated speech text
service under a disposable **standard-user** account. It is not the VoiceType
desktop app and does not record audio or insert text. The packaged DLL is x64.

Extract the zip to a dedicated folder. In an interactive Windows 11 PowerShell
session opened as that standard user, choose the language already active for
your keyboard (`0409` for English US or `0404` for Traditional Chinese) and run:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\desktop\windows-tsf\Invoke-TsfBProbe.ps1 -Language 0409
```

The script refuses a non-clean starting state, registers only the probe's own
CLSID/speech profile, requests session activation, launches a separate x64 ACP
target, and checks `ACTIVATE`, `HELLO`, and the preserved-key callback. Its
`finally` block requests session deactivation/unregistration, checks that the
own profile/category/COM key is absent, launches another target that must not
load the probe, and compares the original keyboard/IME identity. If a public
API rejects cleanup or state cannot be verified, the script reports failure
instead of claiming recovery. Do not rerun a failed probe until its remaining
own-state output is inspected.

Read-only status, before or after the run:

```powershell
$dll = (Resolve-Path .\.scratch\tsf-native\build\Release\voicetype_tsf_probe.dll).Path
& .\.scratch\tsf-native\build\Release\voicetype_tsf_registrar.exe status $dll 0409
```

Diagnostic metadata logs are created under `.scratch/tsf-b/`; they contain
events, PID/TID, opaque context serial, focus epoch and HRESULT, no transcript
or pointer values. Keep the terminal output and these logs if the check fails.
The package's `SHA256SUMS.txt` lists the executable and DLL hashes; the
workflow prints the zip SHA-256. A synthetic callback in this target does not
prove physical Ctrl+CapsLock, coexistence in Notepad/Edge, original-field
delivery, or microphone behavior.
