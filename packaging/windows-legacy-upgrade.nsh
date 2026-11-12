; Electron-builder's UUIDv5 for com.ytultimate.desktop. This is intentionally
; limited to the original per-user 4.x application, not arbitrary uninstallers.
!define YTU_LEGACY_KEY "Software\d5cdec52-ef34-5136-a778-fe870ed73415"
!define YTU_LEGACY_UNINSTALL "Software\Microsoft\Windows\CurrentVersion\Uninstall\d5cdec52-ef34-5136-a778-fe870ed73415"

Function YtuFindLegacyElectron
  StrCpy $YtuLegacyDirectory ""
  StrCpy $YtuLegacyRegistryView 64
  SetRegView 64
  ReadRegStr $1 HKCU "${YTU_LEGACY_UNINSTALL}" "DisplayVersion"
  ReadRegStr $0 HKCU "${YTU_LEGACY_UNINSTALL}" "DisplayName"
  StrCmp $0 "YT Ultimate" legacy_validate
  ; electron-builder defaults to "${productName} ${version}" in Programs.
  StrCmp $0 "YT Ultimate $1" legacy_validate
  StrCpy $YtuLegacyRegistryView 32
  SetRegView 32
  ReadRegStr $1 HKCU "${YTU_LEGACY_UNINSTALL}" "DisplayVersion"
  ReadRegStr $0 HKCU "${YTU_LEGACY_UNINSTALL}" "DisplayName"
  StrCmp $0 "YT Ultimate" legacy_validate
  StrCmp $0 "YT Ultimate $1" legacy_validate legacy_not_found
legacy_validate:
  ReadRegStr $0 HKCU "${YTU_LEGACY_UNINSTALL}" "DisplayVersion"
  StrCpy $0 $0 2
  StrCmp $0 "4." 0 legacy_not_found
  ReadRegStr $YtuLegacyDirectory HKCU "${YTU_LEGACY_KEY}" "InstallLocation"
  StrCmp $YtuLegacyDirectory "" legacy_not_found
  IfFileExists "$YtuLegacyDirectory\YT Ultimate.exe" 0 legacy_not_found
  IfFileExists "$YtuLegacyDirectory\Uninstall YT Ultimate.exe" legacy_found legacy_not_found
legacy_not_found:
  StrCpy $YtuLegacyDirectory ""
legacy_found:
  SetRegView 64
FunctionEnd

; This section is placed after WebView2, before any application files are copied.
; The original uninstaller preserves AppData by configuration and --updated.
Section "Upgrade Electron 4.x"
  StrCmp $YtuLegacyDirectory "" legacy_upgrade_done
  nsis_tauri_utils::FindProcess "YT Ultimate.exe"
  Pop $0
  StrCmp $0 0 legacy_app_running
  DetailPrint "Actualizando YT Ultimate 4.x y conservando el perfil y la biblioteca..."
  ClearErrors
  ExecWait '"$YtuLegacyDirectory\Uninstall YT Ultimate.exe" /S --updated --keep-shortcuts /currentuser' $0
  IfErrors legacy_uninstall_failed
  StrCmp $0 0 0 legacy_uninstall_failed
  ; NSIS may move its uninstaller to TEMP. Wait for its child to remove both
  ; program and registration; never erase the old installation manually.
  StrCpy $1 0
legacy_wait:
  Sleep 500
  IntOp $1 $1 + 1
  IfFileExists "$YtuLegacyDirectory\YT Ultimate.exe" legacy_wait_again
  ${If} $YtuLegacyRegistryView == 32
    SetRegView 32
  ${Else}
    SetRegView 64
  ${EndIf}
  ReadRegStr $0 HKCU "${YTU_LEGACY_UNINSTALL}" "UninstallString"
  SetRegView 64
  StrCmp $0 "" legacy_upgrade_done
legacy_wait_again:
  IntCmp $1 240 legacy_uninstall_failed legacy_wait legacy_uninstall_failed
legacy_app_running:
  MessageBox MB_OK|MB_ICONEXCLAMATION "Cierra YT Ultimate 4.x antes de instalar esta versión. No se ha desinstalado la aplicación anterior." /SD IDOK
  SetErrorLevel 3
  Abort
legacy_uninstall_failed:
  SetRegView 64
  MessageBox MB_OK|MB_ICONEXCLAMATION "No se pudo completar la retirada de YT Ultimate 4.x. La instalación nueva se ha detenido para evitar mezclar ambas versiones. El perfil y la biblioteca no se eliminan." /SD IDOK
  SetErrorLevel 4
  Abort
legacy_upgrade_done:
SectionEnd
