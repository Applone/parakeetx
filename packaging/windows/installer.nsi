Unicode true
!include "MUI2.nsh"
!include "x64.nsh"

!ifndef PACKAGE_STAGE
  !error "PACKAGE_STAGE is required"
!endif
!ifndef PACKAGE_VERSION
  !error "PACKAGE_VERSION is required"
!endif
!ifndef PACKAGE_OUTPUT
  !error "PACKAGE_OUTPUT is required"
!endif

Name "parakeetx"
OutFile "${PACKAGE_OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\parakeetx"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "parakeetx"
ShowInstDetails show
ShowUninstDetails show

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\parakeetx"
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${PACKAGE_STAGE}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "parakeetx requires 64-bit Windows."
    SetErrorLevel 1
    Quit
  ${EndIf}
  SetRegView 64
  SetShellVarContext current
FunctionEnd

Section "parakeetx"
  SetOutPath "$INSTDIR"
  File /r "${PACKAGE_STAGE}\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\parakeetx"
  CreateShortcut "$SMPROGRAMS\parakeetx\parakeetx.lnk" "$INSTDIR\parakeetx.exe"
  CreateShortcut "$SMPROGRAMS\parakeetx\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "parakeetx"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${PACKAGE_VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "parakeetx contributors"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\parakeetx.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetRegView 64
  SetShellVarContext current
  Delete "$SMPROGRAMS\parakeetx\parakeetx.lnk"
  Delete "$SMPROGRAMS\parakeetx\Uninstall.lnk"
  RMDir "$SMPROGRAMS\parakeetx"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  # Delete only installed application files; recordings and settings live elsewhere.
  Delete "$INSTDIR\parakeetx.exe"
  Delete "$INSTDIR\*.dll"
  Delete "$INSTDIR\LICENSE"
  RMDir /r "$INSTDIR\docs"
  RMDir /r "$INSTDIR\python"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
