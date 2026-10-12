; SPDX-License-Identifier: GPL-3.0-or-later
; Copyright (C) 2026 Huang Zhaobin
;
; Per-user installer for the staged Windows tree. Built by tools/packaging/windows-release.py:
;
;     makensis -INPUTCHARSET UTF8 -DVERSION=... -DVERSION_QUAD=... \
;         -DLICENSE=... -DICON=... -DOUTFILE=... -DFILES_NSH=... \
;         packaging/windows/installer.nsi
;
; Installs under %LOCALAPPDATA%\Programs\mirai without elevation. Uninstall deletes only
; the compiled-in list of shipped paths. Upgrade accepts only the registered installation
; and runs its uninstaller, never a mutable file list. User data is not touched.
; A running program is detected by a write-access probe; nothing is renamed or killed.

Unicode true
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "WinMessages.nsh"

!ifndef VERSION
!error "VERSION is required"
!endif
!ifndef VERSION_QUAD
!error "VERSION_QUAD is required"
!endif
!ifndef LICENSE
!error "LICENSE is required"
!endif
!ifndef ICON
!error "ICON is required"
!endif
!ifndef OUTFILE
!error "OUTFILE is required"
!endif
!ifndef FILES_NSH
!error "FILES_NSH is required"
!endif

!define APPID "io.github.zhaob1n.Mirai"
!define PROGID "${APPID}.sgf"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPID}"
!define SHCNE_ASSOCCHANGED 0x08000000
!define MARKER_REL "share\mirai\install.marker"
!define MARKER_TEXT "mirai-owned-install 1"

!include "${FILES_NSH}"

Name "mirai"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\mirai"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
RequestExecutionLevel user
BrandingText "mirai ${VERSION}"

VIProductVersion "${VERSION_QUAD}"
VIAddVersionKey "ProductName" "mirai"
VIAddVersionKey "FileDescription" "mirai installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Copyright (C) 2026 Huang Zhaobin. GPL-3.0-or-later."
VIAddVersionKey "OriginalFilename" "mirai-setup.exe"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\bin\mirai.exe"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${LICENSE}"
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE CheckInstDir
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

LangString DirInUse ${LANG_ENGLISH} "$INSTDIR already holds other files. Choose an empty folder, or the folder of an earlier mirai installation made by this installer."
LangString DirInUse ${LANG_SIMPCHINESE} "$INSTDIR 里已有其他文件。请选择一个空文件夹，或本安装程序以前安装 mirai 的文件夹。"
LangString Forbidden ${LANG_ENGLISH} "$INSTDIR is a Windows system folder. Choose a folder under your user profile."
LangString Forbidden ${LANG_SIMPCHINESE} "$INSTDIR 是 Windows 系统文件夹。请选择用户目录下的文件夹。"
LangString RecordBad ${LANG_ENGLISH} "This folder looks like a mirai installation, but its install record is missing or was altered. Nothing was deleted. Choose an empty folder."
LangString RecordBad ${LANG_SIMPCHINESE} "这个文件夹像是 mirai 的安装目录，但安装记录缺失或已被改动。没有删除任何文件。请选择空文件夹。"
LangString UpgradeStopped ${LANG_ENGLISH} "Could not remove the previous installation safely. Close mirai and run this installer again, or delete $INSTDIR by hand."
LangString UpgradeStopped ${LANG_SIMPCHINESE} "无法安全移除上一版安装。请关闭 mirai 后重新运行本安装程序，或手动删除 $INSTDIR。"
LangString MiraiRunning ${LANG_ENGLISH} "mirai is running from $INSTDIR. Close every mirai window and mirai-server, then choose Retry."
LangString MiraiRunning ${LANG_SIMPCHINESE} "mirai 正在从 $INSTDIR 运行。请关闭所有 mirai 窗口和 mirai-server，然后选择“重试”。"
LangString GdbusRunning ${LANG_ENGLISH} "A background process from this installation is still running. Close it, then choose Retry."
LangString GdbusRunning ${LANG_SIMPCHINESE} "此安装目录里的一个后台进程仍在运行。请关闭它，然后选择“重试”。"
LangString FilesLeft ${LANG_ENGLISH} "Some files in $INSTDIR could not be removed; delete the folder by hand."
LangString FilesLeft ${LANG_SIMPCHINESE} "$INSTDIR 里有些文件无法删除，请手动删除该文件夹。"

Var Locked
Var MarkerOK
Var Registered

!macro DeclareFileLocked UN
Function ${UN}FileLocked
    ; $R0 is the full path. Check replacement access without modifying the file.
    StrCpy $Locked 0
    ${IfNot} ${FileExists} "$R0"
        Return
    ${EndIf}
    System::Call 'kernel32::CreateFileW(w "$R0", i 0x40000000, i 7, p 0, i 3, i 0, p 0) p.r1'
    ${If} $1 = -1
        StrCpy $Locked 1
    ${Else}
        System::Call 'kernel32::CloseHandle(p r1)'
    ${EndIf}
FunctionEnd
!macroend

!macro DeclareTrimNL UN
Function ${UN}TrimNL
    Exch $R0
    ${Do}
        StrCpy $R1 $R0 1 -1
        ${If} $R1 == "$\r"
        ${OrIf} $R1 == "$\n"
            StrCpy $R0 $R0 -1
        ${Else}
            ${Break}
        ${EndIf}
    ${Loop}
    Exch $R0
FunctionEnd
!macroend


!macro DeclarePathUnder UN
Function ${UN}PathUnder
    ; $R0 path, $R1 root. $R2 = 1 when path is the root or a child of it.
    StrCpy $R2 0
    ${If} $R1 == ""
        Return
    ${EndIf}
    StrCpy $R3 $R1 1 -1
    ${If} $R3 == "\"
        StrCpy $R1 $R1 -1
    ${EndIf}
    System::Call 'kernel32::lstrcmpiW(w "$R0", w "$R1") i.r3'
    ${If} $3 = 0
        StrCpy $R2 1
        Return
    ${EndIf}
    StrLen $R3 $R1
    StrCpy $R4 $R0 $R3
    System::Call 'kernel32::lstrcmpiW(w "$R4", w "$R1") i.r3'
    ${If} $3 != 0
        Return
    ${EndIf}
    StrCpy $R4 $R0 1 $R3
    ${If} $R4 == "\"
        StrCpy $R2 1
    ${EndIf}
FunctionEnd
!macroend

!macro DeclareForbidden UN
Function ${UN}ForbiddenDir
    ; $R9 = 1 when $INSTDIR is a drive root or a Windows directory we must never own.
    StrCpy $R9 0
    StrLen $R0 $INSTDIR
    ${If} $R0 < 4
        StrCpy $R9 1
        Return
    ${EndIf}
    StrCpy $R0 $INSTDIR
    StrCpy $R1 "$WINDIR"
    Call ${UN}PathUnder
    ${If} $R2 = 1
        StrCpy $R9 1
        Return
    ${EndIf}
    StrCpy $R0 $INSTDIR
    StrCpy $R1 "$SYSDIR"
    Call ${UN}PathUnder
    ${If} $R2 = 1
        StrCpy $R9 1
        Return
    ${EndIf}
    StrCpy $R0 $INSTDIR
    StrCpy $R1 "$PROGRAMFILES"
    Call ${UN}PathUnder
    ${If} $R2 = 1
        StrCpy $R9 1
        Return
    ${EndIf}
    StrCpy $R0 $INSTDIR
    StrCpy $R1 "$PROGRAMFILES64"
    Call ${UN}PathUnder
    ${If} $R2 = 1
        StrCpy $R9 1
        Return
    ${EndIf}
    StrCpy $R0 $INSTDIR
    StrCpy $R1 "$COMMONFILES"
    Call ${UN}PathUnder
    ${If} $R2 = 1
        StrCpy $R9 1
        Return
    ${EndIf}
    StrCpy $R0 $INSTDIR
    StrCpy $R1 "$COMMONFILES64"
    Call ${UN}PathUnder
    ${If} $R2 = 1
        StrCpy $R9 1
    ${EndIf}
FunctionEnd
!macroend

!macro DeclareStopProcesses UN
Function ${UN}StopOwnedProcesses
    ; Never continues while a file we are about to replace is locked. Nothing is killed.
    ${UN}retry_apps:
    StrCpy $R0 "$INSTDIR\bin\mirai.exe"
    Call ${UN}FileLocked
    ${If} $Locked = 0
        StrCpy $R0 "$INSTDIR\bin\mirai-server.exe"
        Call ${UN}FileLocked
    ${EndIf}
    ${If} $Locked = 1
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "$(MiraiRunning)" /SD IDCANCEL IDRETRY ${UN}retry_apps
        SetErrorLevel 1
        Abort
    ${EndIf}
    ${UN}retry_bus:
    StrCpy $R0 "$INSTDIR\bin\gdbus.exe"
    Call ${UN}FileLocked
    ${If} $Locked = 0
        StrCpy $R0 "$INSTDIR\bin\libgio-2.0-0.dll"
        Call ${UN}FileLocked
    ${EndIf}
    ${If} $Locked = 1
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "$(GdbusRunning)" /SD IDCANCEL IDRETRY ${UN}retry_bus
        SetErrorLevel 1
        Abort
    ${EndIf}
FunctionEnd
!macroend


!macro DeclareMarkerExact UN
Function ${UN}MarkerExact
    StrCpy $MarkerOK 0
    ${IfNot} ${FileExists} "$INSTDIR\${MARKER_REL}"
        Return
    ${EndIf}
    ClearErrors
    FileOpen $0 "$INSTDIR\${MARKER_REL}" r
    ${If} ${Errors}
        Return
    ${EndIf}
    FileSeek $0 0 END $1
    FileSeek $0 0 SET
    FileRead $0 $2
    FileClose $0
    Push $2
    Call ${UN}TrimNL
    Pop $2
    ${If} $2 != "${MARKER_TEXT}"
        Return
    ${EndIf}
    ${If} $1 > 24
        Return
    ${EndIf}
    ${If} $1 < 22
        Return
    ${EndIf}
    StrCpy $MarkerOK 1
FunctionEnd
!macroend

!macro DeclareRegisteredInstall UN
Function ${UN}RegisteredInstall
    StrCpy $Registered 0
    ReadRegStr $R1 HKCU "${UNINSTALL_KEY}" "InstallLocation"
    ${If} $R1 == ""
        Return
    ${EndIf}
    GetFullPathName $R0 "$INSTDIR"
    GetFullPathName $R1 "$R1"
    StrCpy $R2 $R0 1 -1
    ${If} $R2 == "\"
        StrCpy $R0 $R0 -1
    ${EndIf}
    StrCpy $R2 $R1 1 -1
    ${If} $R2 == "\"
        StrCpy $R1 $R1 -1
    ${EndIf}
    System::Call 'kernel32::lstrcmpiW(w "$R0", w "$R1") i.r0'
    ${If} $0 = 0
        StrCpy $Registered 1
    ${EndIf}
FunctionEnd
!macroend

!insertmacro DeclareFileLocked ""
!insertmacro DeclareFileLocked "un."
!insertmacro DeclareTrimNL ""
!insertmacro DeclarePathUnder ""
!insertmacro DeclareForbidden ""
!insertmacro DeclarePathUnder "un."
!insertmacro DeclareForbidden "un."
!insertmacro DeclareRegisteredInstall ""
!insertmacro DeclareRegisteredInstall "un."
!insertmacro DeclareStopProcesses ""
!insertmacro DeclareStopProcesses "un."
!insertmacro DeclareMarkerExact ""

Function CheckInstDir
    Call ForbiddenDir
    ${If} $R9 = 1
        MessageBox MB_ICONEXCLAMATION|MB_OK "$(Forbidden)"
        Abort
    ${EndIf}
    ${DirState} "$INSTDIR" $0
    ${If} $0 == 1
        Call MarkerExact
        Call RegisteredInstall
        ${If} $MarkerOK != 1
        ${OrIf} $Registered != 1
        ${OrIfNot} ${FileExists} "$INSTDIR\uninstall.exe"
            MessageBox MB_ICONEXCLAMATION|MB_OK "$(DirInUse)"
            Abort
        ${EndIf}
    ${EndIf}
FunctionEnd

Function .onInit
    SetShellVarContext current
FunctionEnd

Function un.onInit
    SetShellVarContext current
    Call un.ForbiddenDir
    ${If} $R9 = 1
        SetErrorLevel 1
        Abort
    ${EndIf}
    Call un.RegisteredInstall
    ${If} $R1 != ""
    ${AndIf} $Registered != 1
        SetErrorLevel 1
        Abort
    ${EndIf}
FunctionEnd

Section "mirai"
    SetShellVarContext current
    Call ForbiddenDir
    ${If} $R9 = 1
        MessageBox MB_ICONSTOP|MB_OK "$(Forbidden)" /SD IDOK
        SetErrorLevel 1
        Abort
    ${EndIf}
    ${DirState} "$INSTDIR" $0
    ${If} $0 == 1
        Call MarkerExact
        Call RegisteredInstall
        ${If} $MarkerOK != 1
        ${OrIf} $Registered != 1
        ${OrIfNot} ${FileExists} "$INSTDIR\uninstall.exe"
            MessageBox MB_ICONSTOP|MB_OK "$(RecordBad)" /SD IDOK
            SetErrorLevel 1
            Abort
        ${EndIf}
        Call StopOwnedProcesses
        ExecWait '"$INSTDIR\uninstall.exe" /S _?=$INSTDIR' $0
        ${If} $0 != 0
            MessageBox MB_ICONSTOP|MB_OK "$(UpgradeStopped)" /SD IDOK
            SetErrorLevel 1
            Abort
        ${EndIf}
        Delete "$INSTDIR\uninstall.exe"
    ${EndIf}
    !insertmacro MiraiInstallFiles
    WriteUninstaller "$INSTDIR\uninstall.exe"

    CreateShortcut "$SMPROGRAMS\mirai.lnk" "$INSTDIR\bin\mirai.exe"

    WriteRegStr HKCU "Software\Classes\${PROGID}" "" "SGF game record"
    WriteRegStr HKCU "Software\Classes\${PROGID}\DefaultIcon" "" "$INSTDIR\bin\mirai.exe,0"
    WriteRegStr HKCU "Software\Classes\${PROGID}\shell\open\command" "" '"$INSTDIR\bin\mirai.exe" "%1"'
    WriteRegStr HKCU "Software\Classes\.sgf\OpenWithProgids" "${PROGID}" ""
    WriteRegStr HKCU "Software\Classes\Applications\mirai.exe" "FriendlyAppName" "mirai"
    WriteRegStr HKCU "Software\Classes\Applications\mirai.exe\shell\open\command" "" '"$INSTDIR\bin\mirai.exe" "%1"'
    WriteRegStr HKCU "Software\Classes\Applications\mirai.exe\SupportedTypes" ".sgf" ""
    # HKCR merges the per-user and machine-wide classes: a Go editor installed for all
    # users keeps the default, and mirai is only offered under Open with.
    ReadRegStr $0 HKCR ".sgf" ""
    ${If} $0 == ""
        WriteRegStr HKCU "Software\Classes\.sgf" "" "${PROGID}"
    ${EndIf}

    ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "mirai"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\bin\mirai.exe,0"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Huang Zhaobin"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/zhaob1n/mirai"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1

    System::Call 'shell32::SHChangeNotify(i ${SHCNE_ASSOCCHANGED}, i 0, p 0, p 0)'
SectionEnd

Section "Uninstall"
    SetShellVarContext current
    Call un.StopOwnedProcesses
    !insertmacro MiraiUninstallFiles
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"

    ${If} $Registered = 1
        Delete "$SMPROGRAMS\mirai.lnk"
        DeleteRegKey HKCU "Software\Classes\${PROGID}"
        DeleteRegValue HKCU "Software\Classes\.sgf\OpenWithProgids" "${PROGID}"
        ReadRegStr $0 HKCU "Software\Classes\.sgf" ""
        ${If} $0 == "${PROGID}"
            DeleteRegValue HKCU "Software\Classes\.sgf" ""
        ${EndIf}
        DeleteRegKey HKCU "Software\Classes\Applications\mirai.exe"
        DeleteRegKey HKCU "${UNINSTALL_KEY}"
    ${EndIf}

    ${un.DirState} "$INSTDIR" $0
    ${If} $0 == 1
        MessageBox MB_ICONEXCLAMATION|MB_OK "$(FilesLeft)" /SD IDOK
    ${EndIf}

    System::Call 'shell32::SHChangeNotify(i ${SHCNE_ASSOCCHANGED}, i 0, p 0, p 0)'
SectionEnd
