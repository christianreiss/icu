; Transactional file replacement shared by manual setup and the ICU broker.
; Only fixed program files are touched. User profiles and unknown files stay.
Var UpdateNonce
Var Stage
Var OldVersion
Var OldSize
Var InstallMutex
Var EventStaged
Var EventParent
Var EventReplaced
Var EventHealthy
Var EventRollback
Var EventFailed
Var RestoreFailed
Var SwapAttempts

!macro OpenUpdateEvent kind variable
  System::Call 'kernel32::OpenEventW(i 0x1f0003, i 0, w "Local\ICU.Update.$UpdateNonce.${kind}") p.r0'
  StrCpy ${variable} $0
  ${If} ${variable} == 0
    SetErrorLevel 20
    Abort "Die Update-Sitzung ist nicht mehr verfügbar."
  ${EndIf}
!macroend

Function UpdateInit
  ; Existence prevents concurrent manual installers as well as auto-updates.
  System::Call 'kernel32::CreateMutexW(p 0, i 0, w "Global\ICU.Setup.Transaction") p.r0 ?e'
  Pop $1
  StrCpy $InstallMutex $0
  ${If} $0 == 0
  ${OrIf} $1 == 183
    SetErrorLevel 21
    Abort "Eine andere ICU-Installation läuft bereits."
  ${EndIf}
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/ICUUpdate=" $UpdateNonce
  ${IfNot} ${Errors}
    StrLen $1 $UpdateNonce
    ${If} $1 != 32
      Abort "Ungültige Update-Sitzung."
    ${EndIf}
    StrCpy $1 0
    nonce_loop:
      StrCpy $2 $UpdateNonce 1 $1
      ${If} $2 == ""
        Goto nonce_done
      ${EndIf}
      StrCpy $3 0
      nonce_hex:
        StrCpy $4 "0123456789abcdefABCDEF" 1 $3
        ${If} $4 == ""
          Abort "Ungültige Update-Sitzung."
        ${EndIf}
        ${If} $2 == $4
          IntOp $1 $1 + 1
          Goto nonce_loop
        ${EndIf}
        IntOp $3 $3 + 1
        Goto nonce_hex
    nonce_done:
    ReadRegStr $INSTDIR HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "InstallLocation"
    ${If} $INSTDIR == ""
      Abort "Die bisherige Installation wurde nicht gefunden."
    ${EndIf}
    !insertmacro OpenUpdateEvent staged $EventStaged
    !insertmacro OpenUpdateEvent parent-exited $EventParent
    !insertmacro OpenUpdateEvent replaced $EventReplaced
    !insertmacro OpenUpdateEvent healthy $EventHealthy
    !insertmacro OpenUpdateEvent rollback $EventRollback
    !insertmacro OpenUpdateEvent failed $EventFailed
    ; A validated broker transaction never requires a second setup wizard.
    SetSilent silent
  ${Else}
    StrCpy $UpdateNonce ""
  ${EndIf}
FunctionEnd

; Waiting is bounded. The broker alone decides when the exact parent handle
; exited, so PID reuse cannot redirect this installer to another process.
!macro WaitUpdateEvent event ticks failure
  StrCpy $2 0
  ${Do}
    System::Call 'kernel32::WaitForSingleObject(p ${event}, i 0) i.r0'
    ${If} $0 == 0
      ${ExitDo}
    ${EndIf}
    System::Call 'kernel32::WaitForSingleObject(p $EventRollback, i 0) i.r0'
    ${If} $0 == 0
      Goto ${failure}
    ${EndIf}
    IntOp $2 $2 + 1
    ${If} $2 >= ${ticks}
      Goto ${failure}
    ${EndIf}
    Sleep 100
  ${Loop}
!macroend

!macro FlushFile path failure
  System::Call 'kernel32::CreateFileW(w "${path}", i 0x40000000, i 1, p 0, i 3, i 0, p 0) p.r0'
  ${If} $0 == -1
    Goto ${failure}
  ${EndIf}
  System::Call 'kernel32::FlushFileBuffers(p r0) i.r1'
  System::Call 'kernel32::CloseHandle(p r0)'
  ${If} $1 == 0
    Goto ${failure}
  ${EndIf}
!macroend

!macro BackupFile name
  !insertmacro FlushFile "$Stage\new\${name}" stage_failed
  ${If} ${FileExists} "$INSTDIR\${name}"
    ClearErrors
    CopyFiles /SILENT "$INSTDIR\${name}" "$Stage\old\${name}"
    ${If} ${Errors}
      Goto stage_failed
    ${EndIf}
    !insertmacro FlushFile "$Stage\old\${name}" stage_failed
  ${EndIf}
!macroend

!macro SwapFile name
  ; Persist the intent BEFORE the exchange; recovery also covers the special
  ; partial-failure outcomes documented for ReplaceFileW (1176/1177).
  ClearErrors
  FileOpen $0 "$Stage\swap-${name}" w
  FileWrite $0 "replace"
  FileClose $0
  ${If} ${Errors}
    Goto rollback
  ${EndIf}
  !insertmacro FlushFile "$Stage\swap-${name}" rollback
  StrCpy $SwapAttempts 0
  ${Do}
  ${If} ${FileExists} "$INSTDIR\${name}"
    System::Call 'kernel32::ReplaceFileW(w "$INSTDIR\${name}", w "$Stage\new\${name}", p 0, i 0, p 0, p 0) i.r0'
  ${Else}
    System::Call 'kernel32::MoveFileExW(w "$Stage\new\${name}", w "$INSTDIR\${name}", i 8) i.r0'
  ${EndIf}
    ${If} $0 != 0
      ${ExitDo}
    ${EndIf}
    IntOp $SwapAttempts $SwapAttempts + 1
    ${If} $SwapAttempts >= 50
      ${ExitDo}
    ${EndIf}
    Sleep 100
  ${Loop}
  ${If} $0 == 0
    Goto rollback
  ${EndIf}
!macroend

!macro RestoreFile name
  ${If} ${FileExists} "$Stage\swap-${name}"
    ${If} ${FileExists} "$Stage\old\${name}"
      StrCpy $SwapAttempts 0
      ${Do}
      ${If} ${FileExists} "$INSTDIR\${name}"
        System::Call 'kernel32::ReplaceFileW(w "$INSTDIR\${name}", w "$Stage\old\${name}", p 0, i 0, p 0, p 0) i.r0'
      ${Else}
        System::Call 'kernel32::MoveFileExW(w "$Stage\old\${name}", w "$INSTDIR\${name}", i 8) i.r0'
      ${EndIf}
        ${If} $0 != 0
          ${ExitDo}
        ${EndIf}
        IntOp $SwapAttempts $SwapAttempts + 1
        ${If} $SwapAttempts >= 50
          ${ExitDo}
        ${EndIf}
        Sleep 100
      ${Loop}
      ${If} $0 == 0
        StrCpy $RestoreFailed 1
      ${Else}
        Delete "$Stage\swap-${name}"
      ${EndIf}
    ${Else}
      ; If the staged new file still exists, its exchange never happened.
      ${IfNot} ${FileExists} "$Stage\new\${name}"
        ClearErrors
        Delete "$INSTDIR\${name}"
        ${If} ${Errors}
          StrCpy $RestoreFailed 1
        ${Else}
          Delete "$Stage\swap-${name}"
        ${EndIf}
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

Function RestoreProgramFiles
  StrCpy $RestoreFailed 0
  !insertmacro RestoreFile icu.exe
  !insertmacro RestoreFile LICENSE
  !insertmacro RestoreFile THIRD-PARTY-LICENSES.txt
  !insertmacro RestoreFile NSIS-COPYING.txt
  !insertmacro RestoreFile uninstall.exe
  ${If} $OldVersion != ""
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "DisplayVersion" "$OldVersion"
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "EstimatedSize" $OldSize
  ${EndIf}
FunctionEnd
