; SPDX-License-Identifier: GPL-3.0-or-later
; Copyright (C) 2026 Huang Zhaobin
;
; Per-user installer for the staged Windows tree. Built by packaging/windows/stage.sh:
;
;     makensis -DVERSION=... -DSTAGE=<staged tree> -DICON=<mirai.ico> -DOUTFILE=<setup.exe>
;
; Installs under %LOCALAPPDATA%\Programs\mirai without elevation, adds a Start menu entry,
; offers mirai for .sgf files, and registers an uninstaller. User data in %APPDATA% and
; %LOCALAPPDATA%\zhaob1n is left alone on uninstall, as a package manager would.

Unicode true
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define APPID "io.github.zhaob1n.Mirai"
!define PROGID "${APPID}.sgf"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPID}"
!define SHCNE_ASSOCCHANGED 0x08000000

Name "mirai"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\mirai"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
RequestExecutionLevel user
BrandingText "mirai ${VERSION}"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\bin\mirai.exe"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE}/doc/LICENSE"
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE CheckInstDir
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

LangString DirInUse ${LANG_ENGLISH} "$INSTDIR already holds other files. Choose an empty folder, or the folder of an earlier mirai installation."
LangString DirInUse ${LANG_SIMPCHINESE} "$INSTDIR 里已有其他文件。请选择一个空文件夹，或以前安装 mirai 的文件夹。"
LangString MiraiRunning ${LANG_ENGLISH} "mirai is running from $INSTDIR. Close every mirai window, then choose Retry."
LangString MiraiRunning ${LANG_SIMPCHINESE} "mirai 正在从 $INSTDIR 运行。请关闭所有 mirai 窗口，然后选择“重试”。"
LangString FilesLeft ${LANG_ENGLISH} "Some files in $INSTDIR could not be removed; delete the folder by hand."
LangString FilesLeft ${LANG_SIMPCHINESE} "$INSTDIR 里有些文件无法删除，请手动删除该文件夹。"

; Sets $R0 to 1 when $INSTDIR is missing, empty, or an earlier mirai installation: the only
; folders whose bin, lib, share and doc this installer may later delete.
Function InstDirUsable
    StrCpy $R0 1
    ${If} ${FileExists} "$INSTDIR\bin\mirai.exe"
        Return
    ${EndIf}
    FindFirst $R1 $R2 "$INSTDIR\*.*"
    ${DoWhile} $R2 != ""
        ${If} $R2 != "."
        ${AndIf} $R2 != ".."
            StrCpy $R0 0
            ${Break}
        ${EndIf}
        FindNext $R1 $R2
    ${Loop}
    FindClose $R1
FunctionEnd

Function CheckInstDir
    Call InstDirUsable
    ${If} $R0 != 1
        MessageBox MB_ICONEXCLAMATION|MB_OK "$(DirInUse)"
        Abort
    ${EndIf}
FunctionEnd

; Files in bin\ are held open by a running mirai and by GLib's private session bus,
; bin\gdbus.exe, which outlives mirai. mirai may hold an unsaved record, so it is never
; stopped: the user is asked to close it. The bus is stopped, but only the one started from
; this $INSTDIR; another program's is left alone. The directory reaches PowerShell through
; the environment, never inside its quoted command, so no character in a path can break it.
!macro StopProcesses
    System::Call 'kernel32::SetEnvironmentVariable(t "MIRAI_INSTDIR", t "$INSTDIR")'
    retry:
    nsExec::Exec `powershell.exe -NoProfile -NonInteractive -Command "if (Get-Process -Name mirai -ErrorAction SilentlyContinue | Where-Object { $$_.Path -and $$_.Path.StartsWith($$env:MIRAI_INSTDIR + '\', 'OrdinalIgnoreCase') }) { exit 1 }"`
    Pop $0
    ${If} $0 == 1
        MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "$(MiraiRunning)" /SD IDCANCEL IDRETRY retry
        Abort
    ${EndIf}
    nsExec::Exec `powershell.exe -NoProfile -NonInteractive -Command "Get-Process -Name gdbus -ErrorAction SilentlyContinue | Where-Object { $$_.Path -and $$_.Path.StartsWith($$env:MIRAI_INSTDIR + '\', 'OrdinalIgnoreCase') } | ForEach-Object { $$_.Kill(); $$_.WaitForExit(5000) | Out-Null }"`
    Pop $0
!macroend

; The directories this installer creates; never $INSTDIR itself with its contents.
!macro RemoveTree
    RMDir /r "$INSTDIR\bin"
    RMDir /r "$INSTDIR\lib"
    RMDir /r "$INSTDIR\share"
    RMDir /r "$INSTDIR\doc"
!macroend

Section "mirai"
    SetShellVarContext current
    ; The directory page checks this too, but a silent install with /D= skips that page.
    Call InstDirUsable
    ${If} $R0 != 1
        MessageBox MB_ICONSTOP|MB_OK "$(DirInUse)" /SD IDOK
        Abort
    ${EndIf}
    !insertmacro StopProcesses
    SetOutPath "$INSTDIR"
    ; An upgrade must not keep a DLL the new build no longer ships.
    !insertmacro RemoveTree
    File /r "${STAGE}/*"
    WriteUninstaller "$INSTDIR\uninstall.exe"

    CreateShortcut "$SMPROGRAMS\mirai.lnk" "$INSTDIR\bin\mirai.exe"

    WriteRegStr HKCU "Software\Classes\${PROGID}" "" "SGF game record"
    WriteRegStr HKCU "Software\Classes\${PROGID}\DefaultIcon" "" "$INSTDIR\bin\mirai.exe,0"
    WriteRegStr HKCU "Software\Classes\${PROGID}\shell\open\command" "" '"$INSTDIR\bin\mirai.exe" "%1"'
    WriteRegStr HKCU "Software\Classes\.sgf\OpenWithProgids" "${PROGID}" ""
    WriteRegStr HKCU "Software\Classes\Applications\mirai.exe\SupportedTypes" ".sgf" ""
    ; The default only when nothing else claimed .sgf; Windows keeps the user's own choice
    ; elsewhere and this never overrides it.
    ReadRegStr $0 HKCU "Software\Classes\.sgf" ""
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
    !insertmacro StopProcesses
    Delete "$SMPROGRAMS\mirai.lnk"
    !insertmacro RemoveTree
    ${If} ${FileExists} "$INSTDIR\bin\*.*"
        MessageBox MB_ICONEXCLAMATION|MB_OK "$(FilesLeft)" /SD IDOK
    ${EndIf}
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"

    DeleteRegKey HKCU "Software\Classes\${PROGID}"
    DeleteRegValue HKCU "Software\Classes\.sgf\OpenWithProgids" "${PROGID}"
    ReadRegStr $0 HKCU "Software\Classes\.sgf" ""
    ${If} $0 == "${PROGID}"
        DeleteRegValue HKCU "Software\Classes\.sgf" ""
    ${EndIf}
    DeleteRegKey HKCU "Software\Classes\Applications\mirai.exe"
    DeleteRegKey HKCU "${UNINSTALL_KEY}"

    System::Call 'shell32::SHChangeNotify(i ${SHCNE_ASSOCCHANGED}, i 0, p 0, p 0)'
SectionEnd
