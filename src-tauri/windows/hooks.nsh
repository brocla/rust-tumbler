; NSIS installer hooks (bundle.windows.nsis.installerHooks in tauri.conf.json).
;
; Puts the install folder on the per-user PATH, so `tumbler-cli` runs from any
; terminal, and takes it off again on uninstall (doc/cli-plan.html, change #9).
;
; The PATH edit itself is done by tumbler-cli.exe (`__path add|remove`), not
; here: NSIS strings hold at most 1024 characters, and a longer PATH read
; through them comes back cut short, so writing it back would silently drop
; other programs' entries. tumbler-cli reads and writes the registry value
; whole, keeps its type and any %VAR% references, and touches only this
; folder's entry. A failure is ignored: the install still works, just without
; tumbler-cli on PATH.
;
; Tauri runs the old uninstaller (with /UPDATE) before installing an update, so
; an upgrade removes the entry and then adds it back.

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec '"$INSTDIR\tumbler-cli.exe" __path add "$INSTDIR"'
  Pop $0
  ; Tell running programs (Explorer, so new terminals) the environment changed.
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

; Before the files are deleted, while tumbler-cli.exe still exists.
!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec '"$INSTDIR\tumbler-cli.exe" __path remove "$INSTDIR"'
  Pop $0
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend
