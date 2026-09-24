; Stop the running app before NSIS replaces the installation directory during
; an upgrade. The yohaku-core-node.exe line only matters when upgrading FROM a
; legacy release that still shipped the Node sidecar (its files stay locked
; while the old process is alive); keep it until those installs age out.
!macro NSIS_HOOK_PREINSTALL
  nsExec::ExecToLog 'taskkill.exe /F /T /IM yohaku-core-node.exe'
  nsExec::ExecToLog 'taskkill.exe /F /IM "Yohaku Companion.exe"'
  Sleep 1500
!macroend
