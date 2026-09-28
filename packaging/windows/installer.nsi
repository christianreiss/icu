; Per-user NSIS installer for icu.exe. Built by packaging/windows/installer.sh
; from the exact files packaging/windows/build.sh already staged for the
; portable ZIP (target/pkg/windows/icu-<ver>-windows-x86_64/); do not invoke
; makensis on this file directly outside that script — VERSION, VIVERSION,
; STAGE, OUTFILE, ICON and NOTICE are passed in as -D command-line defines.
;
; RequestExecutionLevel user embeds an asInvoker manifest: this is what
; stops Windows' installer-name heuristic (files matching *setup*.exe etc.)
; from popping a UAC prompt anyway, matching the no-admin portable .exe and
; the HKCU-only registry writes crates/icu-client/src/winreg.rs already
; makes at runtime (.icu file association, ICU.Invite ProgID, toast
; AppUserModelId "ICU").
;
; This host only has the amd64-unicode NSIS stubs installed (Fedora package
; mingw64-nsis, no mingw32-nsis), so the target must be selected explicitly.
Unicode true
Target amd64-unicode

!ifndef VERSION
  !error "pass -DVERSION=x.y.z"
!endif
!ifndef VIVERSION
  !error "pass -DVIVERSION=x.y.z.0 (4 numeric parts, computed like crates/icu-client/build.rs does for the exe's own version resource)"
!endif
!ifndef STAGE
  !error "pass -DSTAGE=<packaging/windows/build.sh staging dir: icu.exe, LICENSE, THIRD-PARTY-LICENSES.txt>"
!endif
!ifndef OUTFILE
  !error "pass -DOUTFILE=<path>-setup.exe"
!endif
!ifndef ICON
  !error "pass -DICON=<path to icu.ico>"
!endif
!ifndef NOTICE
  !error "pass -DNOTICE=<path to packaging/windows/notices/nsis-COPYING>"
!endif

Name "ICU"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\ICU"
InstallDirRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "InstallLocation"
RequestExecutionLevel user
; Keeps the compiled stub under the zlib/libpng license only (see
; packaging/windows/notices/nsis-COPYING): the LZMA module is Common Public
; License 1.0 and the bzip2 module has its own license; neither is linked
; into the stub when the compressor is zlib.
SetCompressor /SOLID zlib
ShowInstDetails show
ShowUnInstDetails show

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define MUI_ICON   "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE}\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "German"
!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${VIVERSION}"
VIAddVersionKey "ProductName"     "ICU"
VIAddVersionKey "FileDescription" "ICU Setup"
VIAddVersionKey "FileVersion"     "${VERSION}"
VIAddVersionKey "ProductVersion"  "${VERSION}"
VIAddVersionKey "LegalCopyright"  "Apache-2.0"

; Windows refuses to delete or overwrite the image of a process that is
; running, so this is how the installer/uninstaller detects "ICU is still
; running" without needing to know the app's single-instance mutex name
; (crates/icu-client/src/instance.rs: Local\icu-<sha256(config dir)[:8]>,
; content-addressed, not a fixed name — not something to replicate here).
!macro RefuseIfRunning path
  ${If} ${FileExists} "${path}"
    ClearErrors
    Delete "${path}"
    ${If} ${Errors}
      MessageBox MB_OK|MB_ICONEXCLAMATION \
        "ICU läuft noch (siehe Infobereich der Taskleiste). Bitte beenden und erneut versuchen." /SD IDOK
      Abort
    ${EndIf}
  ${EndIf}
!macroend

Section "ICU (erforderlich)" SEC_APP
  SectionIn RO
  SetOutPath "$INSTDIR"
  !insertmacro RefuseIfRunning "$INSTDIR\icu.exe"
  File "${STAGE}\icu.exe"
  File "${STAGE}\LICENSE"
  File "${STAGE}\THIRD-PARTY-LICENSES.txt"
  File "/oname=NSIS-COPYING.txt" "${NOTICE}"

  CreateDirectory "$SMPROGRAMS"
  ; This NSIS 3.11 build's CreateShortcut only takes the pre-3.08 parameter
  ; set (link, target, params, icon_file, icon_index, showmode, hotkey,
  ; comment) — no workingdir/appUserModelId params (verified: passing them
  ; fails with "CreateShortCut expects 2-9 parameters"). Taskbar grouping
  ; while ICU is running does not depend on the shortcut's AppUserModelId
  ; anyway: main.rs already calls
  ; SetCurrentProcessExplicitAppUserModelID("ICU") on the running process,
  ; which is what Explorer groups by. Verify this holds in practice —
  ; docs/development/testing.md §8.
  CreateShortCut "$SMPROGRAMS\ICU.lnk" "$INSTDIR\icu.exe" "" "$INSTDIR\icu.exe" 0 \
      SW_SHOWNORMAL "" "ICU"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "DisplayName" "ICU"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "InstallLocation" "$INSTDIR"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "DisplayIcon" "$INSTDIR\icu.exe"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "DisplayVersion" "${VERSION}"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "Publisher" "ICU"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "NoRepair" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU" "EstimatedSize" "$0"
SectionEnd

; Unchecked by default: matches the "no automatic clutter, pin manually"
; stance already documented in packaging/windows/README.txt for the ZIP.
Section /o "Desktop-Verknüpfung" SEC_DESKTOP
  CreateShortCut "$DESKTOP\ICU.lnk" "$INSTDIR\icu.exe" "" "$INSTDIR\icu.exe" 0 \
      SW_SHOWNORMAL "" "ICU"
SectionEnd

Section "Uninstall"
  !insertmacro RefuseIfRunning "$INSTDIR\icu.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD-PARTY-LICENSES.txt"
  Delete "$INSTDIR\NSIS-COPYING.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"          ; no /r: never recursively delete a dir we don't fully own

  Delete "$SMPROGRAMS\ICU.lnk"
  Delete "$DESKTOP\ICU.lnk"

  DeleteRegKey HKCU "Software\Classes\.icu"
  DeleteRegKey HKCU "Software\Classes\ICU.Invite"
  DeleteRegKey HKCU "Software\Classes\AppUserModelId\ICU"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU"

  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)' ; SHCNE_ASSOCCHANGED

  ; Default "Nein": the private key under %APPDATA%\icu is DPAPI-encrypted
  ; and is the user's identity/passport (docs/research/packaging-licensing.md,
  ; packaging/windows/README.txt) — an uninstall must not silently strand it.
  MessageBox MB_YESNO|MB_ICONQUESTION \
    "Auch Einstellungen, Protokoll und Identität entfernen?$\n$\n\
    %APPDATA%\icu und %LOCALAPPDATA%\icu\data werden dann unwiderruflich \
    gelöscht. Der private Schlüssel darin meldet dich bei deinem Zugang an \
    – wie ein Passwort behandeln." /SD IDNO IDNO keep_data
    RMDir /r "$APPDATA\icu"
    RMDir /r "$LOCALAPPDATA\icu"
  keep_data:
SectionEnd
