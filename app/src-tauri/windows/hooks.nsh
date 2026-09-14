; The daemon runs out of the install directory, so an update has to stop it
; before its files can be replaced, and start it again afterwards.
;
; `cortisoff stop` goes through the pipe, which is the clean exit: every
; screen is put back before the process ends. Killing it would leave that to
; the journal at the next start.
;
; The stop is asked, then waited for: a fixed sleep once ran out before the
; daemon had put three monitors back, NSIS could not replace a file that was
; still running, and the start hook relaunched the old build without a word.
; A running executable cannot be opened for writing, so the wait is exactly
; that, up to twenty seconds.
;
; Autostart is switched on by a fresh install only. An update leaves the
; logon task exactly as the person left it, on or off.

!macro CORTISOFF_WAIT_STOPPED SUFFIX
  StrCpy $1 0
  cortisoff_wait_loop_${SUFFIX}:
    ClearErrors
    FileOpen $2 "$INSTDIR\cortisoffd.exe" a
    IfErrors cortisoff_wait_locked_${SUFFIX}
    FileClose $2
    Goto cortisoff_wait_done_${SUFFIX}
  cortisoff_wait_locked_${SUFFIX}:
    IntOp $1 $1 + 1
    IntCmp $1 80 cortisoff_wait_done_${SUFFIX} 0 cortisoff_wait_done_${SUFFIX}
    Sleep 250
    Goto cortisoff_wait_loop_${SUFFIX}
  cortisoff_wait_done_${SUFFIX}:
!macroend

!macro NSIS_HOOK_PREINSTALL
  Var /GLOBAL CortisoffUpdate
  StrCpy $CortisoffUpdate "no"
  IfFileExists "$INSTDIR\cortisoffd.exe" 0 cortisoff_preinstall_done
    StrCpy $CortisoffUpdate "yes"
    nsExec::Exec '"$INSTDIR\cortisoff.exe" stop'
    Pop $0
    !insertmacro CORTISOFF_WAIT_STOPPED install
  cortisoff_preinstall_done:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  StrCmp $CortisoffUpdate "yes" cortisoff_postinstall_start 0
    nsExec::Exec '"$INSTDIR\cortisoffd.exe" --autostart on'
    Pop $0
  cortisoff_postinstall_start:
  Exec '"$INSTDIR\cortisoffd.exe"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec '"$INSTDIR\cortisoff.exe" stop'
  Pop $0
  nsExec::Exec '"$INSTDIR\cortisoffd.exe" --autostart off'
  Pop $0
  !insertmacro CORTISOFF_WAIT_STOPPED uninstall
!macroend
