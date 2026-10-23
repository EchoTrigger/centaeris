!macro customInstallMode
  StrCpy $isForceCurrentInstall "1"
!macroend

; Replace the default process termination with a bounded, read-only check.
; Runtime owns idle detection; the installer never cancels runs or kills clients.
!macro customCheckAppRunning
  InitPluginsDir
  File /oname=$PLUGINSDIR\centaeris-preflight.ps1 "${BUILD_RESOURCES_DIR}\preflight.ps1"
  ReadRegStr $R5 HKCU "${INSTALL_REGISTRY_KEY}" InstallLocation
  centaeris_preflight_retry:
  nsExec::ExecToStack /TIMEOUT=30000 '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\centaeris-preflight.ps1" -InstallDirectory "$INSTDIR" -PreviousDirectory "$R5"'
  Pop $0
  Pop $1
  ${If} $0 != 0
    DetailPrint "$1"
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "Centaeris is still in use, or its processes could not be checked.$\r$\n$\r$\nChoose Exit from the Centaeris tray menu, close TUI clients, and finish active tasks. Then retry. Your sessions and workspace files are preserved." /SD IDCANCEL IDRETRY centaeris_preflight_retry
    SetErrorLevel 2
    Quit
  ${EndIf}
!macroend
