; Microsoft documents these registry locations for the Evergreen runtime.
; The bootstrapper is Microsoft's signed redistributable, SHA-256 pinned by xtask.
Function YtuCheckWebView2
  SetRegView 32
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  StrCmp $0 "" check_user_runtime
  StrCmp $0 "0.0.0.0" check_user_runtime runtime_found
check_user_runtime:
  ReadRegStr $0 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  StrCmp $0 "" runtime_missing
  StrCmp $0 "0.0.0.0" runtime_missing runtime_found
runtime_missing:
  StrCpy $0 0
  Goto runtime_check_done
runtime_found:
  StrCpy $0 1
runtime_check_done:
  SetRegView 64
FunctionEnd

Section "WebView2 Runtime"
  SectionIn RO
  Call YtuCheckWebView2
  StrCmp $0 1 runtime_ready
  DetailPrint "Instalando Microsoft Edge WebView2. Se necesita conexión a Internet..."
  InitPluginsDir
  File "/oname=$PLUGINSDIR\MicrosoftEdgeWebView2Setup.exe" "@WEBVIEW2_BOOTSTRAPPER@"
  ClearErrors
  ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebView2Setup.exe" /silent /install' $1
  IfErrors runtime_install_failed
  Call YtuCheckWebView2
  StrCmp $0 1 runtime_ready runtime_install_failed
runtime_install_failed:
  Delete "$PLUGINSDIR\MicrosoftEdgeWebView2Setup.exe"
  MessageBox MB_OK|MB_ICONEXCLAMATION "No se pudo instalar Microsoft Edge WebView2 (código $1). Comprueba tu conexión a Internet y vuelve a ejecutar este instalador. La instalación de YT Ultimate no ha continuado." /SD IDOK
  SetErrorLevel 2
  Abort
runtime_ready:
  Delete "$PLUGINSDIR\MicrosoftEdgeWebView2Setup.exe"
SectionEnd
