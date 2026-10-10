; LoopCut NSIS installer hooks.
;
; FFmpeg is shipped as a versioned resource folder ($INSTDIR\ffmpeg\<version>\). Remove any previous
; FFmpeg folders before installing so a same-version reinstall or an upgrade can never keep stale
; binaries or DLLs around (see tauri-apps/tauri#15134). LoopCut runs every FFmpeg child inside a
; Windows Job Object with KILL_ON_JOB_CLOSE, so closing the app (which the updater does before
; installing) already terminates them; no process killing is needed here.

!macro NSIS_HOOK_PREINSTALL
  RMDir /r "$INSTDIR\ffmpeg"
!macroend

!macro NSIS_HOOK_POSTINSTALL
!macroend

!macro NSIS_HOOK_PREUNINSTALL
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  RMDir /r "$INSTDIR\ffmpeg"
!macroend
