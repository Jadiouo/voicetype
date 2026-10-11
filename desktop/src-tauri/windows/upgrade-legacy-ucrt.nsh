; 0.2.0-alpha.1 shipped 44 Windows 11 UCRT/API-set copies that 0.2.1 no longer
; packages. In-place silent upgrades otherwise leave those old app-owned files
; behind: Tauri's new uninstaller only knows the new resource list.
!macro NSIS_HOOK_PREINSTALL
  ReadRegStr $R0 SHCTX "${UNINSTKEY}" "DisplayVersion"
  ${If} $R0 == "0.2.0-alpha.1"
    ; Confirm both Tauri registry records and the fixed, app-owned old
    ; uninstaller. Never execute an arbitrary UninstallString from registry.
    ReadRegStr $R2 SHCTX "${UNINSTKEY}" "InstallLocation"
    ReadRegStr $R3 SHCTX "${MANUPRODUCTKEY}" ""
    ReadRegStr $R4 SHCTX "${UNINSTKEY}" "UninstallString"
    StrCpy $R5 "$\"$INSTDIR$\""
    ${If} $R2 != $R5
      Goto legacy_upgrade_failed
    ${EndIf}
    ${If} $R3 != $INSTDIR
      Goto legacy_upgrade_failed
    ${EndIf}
    StrCpy $R5 "$\"$INSTDIR\uninstall.exe$\""
    ${If} $R4 != $R5
      Goto legacy_upgrade_failed
    ${EndIf}
    IfFileExists "$INSTDIR\uninstall.exe" 0 legacy_upgrade_failed
    ; Pinned Tauri 2.12.1 uses this synchronous _?= form on upgrades.
    ; /UPDATE keeps AppData, autostart and shortcuts untouched.
    ClearErrors
    ExecWait '"$INSTDIR\uninstall.exe" /S /UPDATE _?=$INSTDIR' $R1
    IfErrors legacy_upgrade_failed
    ${If} $R1 != 0
      Goto legacy_upgrade_failed
    ${EndIf}
    ; The old uninstaller self-file may persist until overwritten. The old
    ; main EXE and all discontinued OS UCRT copies must already be gone.
    IfFileExists "$INSTDIR\voicetype-desktop.exe" legacy_upgrade_failed 0
    IfFileExists "$INSTDIR\spelling\_internal\api-ms-win-*.dll" legacy_upgrade_failed 0
    IfFileExists "$INSTDIR\spelling\_internal\ucrtbase.dll" legacy_upgrade_failed 0
    Goto legacy_upgrade_done
    legacy_upgrade_failed:
      MessageBox MB_OK|MB_ICONSTOP "Could not safely replace the previous VoiceType Preview installation." /SD IDOK
      SetErrorLevel 1
      Quit
    legacy_upgrade_done:
  ${EndIf}
!macroend
