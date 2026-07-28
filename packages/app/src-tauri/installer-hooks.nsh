; The core loads native .node modules. Windows keeps those files locked while
; the old core process is alive, so stop the running app before NSIS replaces
; the installation directory during an upgrade.
!macro NSIS_HOOK_PREINSTALL
  nsExec::ExecToLog 'taskkill.exe /F /T /IM yohaku-core-node.exe'
  nsExec::ExecToLog 'taskkill.exe /F /IM "Yohaku Companion.exe"'
  Sleep 1500
!macroend
