; The Windows installer for Portal Desktop, built by .github/workflows/release.yml:
;   makensis /DVERSION=0.5.3 /DSOURCE=<folder with portal-desktop.exe> /DOUTFILE=<setup.exe> installer.nsi
;
; Installs for the current user only (no administrator rights needed) into
; its own folder, separate from the `portal` command line tool, adds Start
; menu and desktop shortcuts, and an entry in Settings > Apps to remove it.
; `/S` installs silently (install-desktop.ps1 uses that).

Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !error "pass /DVERSION=<version>"
!endif
!ifndef SOURCE
  !error "pass /DSOURCE=<folder with portal-desktop.exe>"
!endif
!ifndef OUTFILE
  !define OUTFILE "portal-desktop-setup.exe"
!endif
; Windows wants only numbers here: 0.6.0 for 0.6.0-rc.1.
!ifndef NUMERIC_VERSION
  !define NUMERIC_VERSION "${VERSION}"
!endif

!define APP "Portal"
!define EXE "portal-desktop.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\PortalDesktop"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Portal Desktop"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "${APP} ${VERSION}"

VIProductVersion "${NUMERIC_VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Marios Stefanidis, MIT OR Apache-2.0"

!define MUI_ICON "..\..\icon\app.ico"
!define MUI_UNICON "..\..\icon\app.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Open Portal"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  ; A running copy would keep the old program file locked.
  nsExec::Exec 'taskkill /IM ${EXE} /F'
  SetOutPath "$INSTDIR"
  File "${SOURCE}\${EXE}"
  File "${SOURCE}\LICENSE-MIT"
  File "${SOURCE}\LICENSE-APACHE"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}"
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${EXE}"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Marios Stefanidis"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/MStefa003/portal"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /IM ${EXE} /F'
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\LICENSE-MIT"
  Delete "$INSTDIR\LICENSE-APACHE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ; Settings in %APPDATA%\portal are shared with the command line tool and kept.
SectionEnd
