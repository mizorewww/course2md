; Per-user Windows installer of the desktop package (Inno Setup 6.3 or later).
; desktop/scripts/windows_installer.py compiles it with these /D definitions:
;   AppId          the installer's identity, a GUID without braces: never change
;                  it, or an upgrade installs a second copy beside the first
;   AppVersion     the product SemVer (3.0.0-alpha.5)
;   FileVersion    its numeric core for the version resource (3.0.0.0)
;   SourceDir      the package directory the portable ZIP holds
;   OutputDir      where the setup program is written
;   OutputName     its file name without .exe
;   IconFile       the application icon
;   Programs       the package's .exe files, relative and joined by |: the only
;                  programs setup and the uninstaller stop
;   Sign           (optional) sign the setup program and its uninstaller with the
;                  SignTool "course2md" given to ISCC as /Scourse2md=<command>
;
; Every file of the package is installed with its relative path unchanged, so
; the app finds its engine and bundled tools beside course2md-desktop.exe as
; it does in the extracted ZIP. The uninstaller removes only what was installed:
; settings (%APPDATA%\course2md), model caches (%LOCALAPPDATA%\course2md) and
; note libraries live elsewhere and stay.
;
; ChineseSimplified.isl and ChineseTraditional.isl are Inno Setup's user-contributed
; translations (jrsoftware/issrc tag is-6_7_1, Files/Languages/Unofficial).
; Modified: ChineseSimplified.isl was saved with a UTF-8 BOM, so the compiler
; reads it as UTF-8; its text is unchanged. ChineseTraditional.isl is unmodified.

#ifndef AppId
  #error AppId must be defined
#endif

[Setup]
AppId={{{#AppId}}
AppName=course2md
AppVersion={#AppVersion}
AppVerName=course2md {#AppVersion}
AppPublisher=course2md
AppPublisherURL=https://github.com/mizorewww/course2md
AppSupportURL=https://github.com/mizorewww/course2md/issues
AppUpdatesURL=https://github.com/mizorewww/course2md/releases
VersionInfoVersion={#FileVersion}
VersionInfoProductVersion={#FileVersion}
VersionInfoProductTextVersion={#AppVersion}
; Per user, without administrator rights: {autopf} is %LOCALAPPDATA%\Programs.
PrivilegesRequired=lowest
DefaultDirName={autopf}\course2md
DisableProgramGroupPage=yes
ShowLanguageDialog=auto
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; Not the Restart Manager: the app's window does not answer its session-end
; messages, and a silent upgrade waited on it indefinitely. [Code] stops
; the package's own programs running from the install instead, before setup
; or the uninstaller touches a file; tasks resume from their checkpoints on the next start.
CloseApplications=no
RestartApplications=no
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\course2md-desktop.exe
UninstallDisplayName=course2md
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
#ifdef Sign
SignTool=course2md
#endif

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "zh_Hans"; MessagesFile: "{#SourcePath}ChineseSimplified.isl"
Name: "zh_Hant"; MessagesFile: "{#SourcePath}ChineseTraditional.isl"
Name: "ja"; MessagesFile: "compiler:Languages\Japanese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; Directories the package owns whole: a new version's bundled yt-dlp and
; llama.cpp replace the old ones instead of mixing with files they dropped.
Type: filesandordirs; Name: "{app}\yt-dlp"
Type: filesandordirs; Name: "{app}\llama.cpp"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\course2md"; Filename: "{app}\course2md-desktop.exe"
Name: "{autodesktop}\course2md"; Filename: "{app}\course2md-desktop.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\course2md-desktop.exe"; Description: "{cm:LaunchProgram,course2md}"; Flags: nowait postinstall skipifsilent

[Code]
// Stops the package's own programs if they run from the install directory:
// only processes whose executable is exactly {app}\<one of Programs>, the
// .exe files the package installs (windows_installer.py lists them), so a
// directory shared with other software never loses its other programs. The
// script and both values reach PowerShell as a file and -File arguments,
// never as code, so no character of the path is interpreted. It waits up to
// 30 s and exits 1 if one is still running.
function StopScript: String;
begin
  Result :=
    'param([string]$Dir, [string]$Programs)' + #13#10 +
    '$ErrorActionPreference = ''SilentlyContinue''' + #13#10 +
    '$targets = @{}' + #13#10 +
    '$files = New-Object -ComObject Scripting.FileSystemObject' + #13#10 +
    'foreach ($name in $Programs.Split(''|'')) {' + #13#10 +
    '  $path = Join-Path $Dir $name' + #13#10 +
    '  $targets[$path.ToLowerInvariant()] = 1' + #13#10 +
    '  try { $targets[$files.GetFile($path).ShortPath.ToLowerInvariant()] = 1 } catch { }' + #13#10 +
    '}' + #13#10 +
    'function Running { @(Get-CimInstance Win32_Process -OperationTimeoutSec 10 | Where-Object {' + #13#10 +
    '  $_.ExecutablePath -and $targets.ContainsKey($_.ExecutablePath.ToLowerInvariant()) }) }' + #13#10 +
    '$deadline = (Get-Date).AddSeconds(30)' + #13#10 +
    'do {' + #13#10 +
    '  $found = Running' + #13#10 +
    '  if ($found.Count -eq 0) { exit 0 }' + #13#10 +
    '  foreach ($process in $found) { Stop-Process -Id $process.ProcessId -Force }' + #13#10 +
    '  Start-Sleep -Milliseconds 250' + #13#10 +
    '} while ((Get-Date) -lt $deadline)' + #13#10 +
    'if ((Running).Count -eq 0) { exit 0 } else { exit 1 }' + #13#10;
end;

procedure StopProgramsIn(Dir: String);
var
  Script: String;
  ResultCode: Integer;
begin
  Dir := RemoveBackslash(Dir);
  // Only a directory that holds this app; a fresh install has nothing to stop.
  if not FileExists(Dir + '\course2md-desktop.exe') then begin
    Log('No course2md install in ' + Dir + '; nothing to stop');
    Exit;
  end;
  Script := AddBackslash(GetTempDir) + 'course2md-stop-programs.ps1';
  if not SaveStringToFile(Script, StopScript, False) then begin
    Log('Could not write ' + Script + '; programs were not stopped');
    Exit;
  end;
  Log('Stopping programs running from ' + Dir);
  if Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
          '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + Script +
          '" -Dir "' + Dir + '" -Programs "{#Programs}"',
          '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    Log('Stopped programs running from the install (exit code ' + IntToStr(ResultCode) + ')')
  else
    Log('Could not run PowerShell to stop programs: ' + SysErrorMessage(ResultCode));
  DeleteFile(Script);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  // On an upgrade {app} is the earlier install's directory.
  StopProgramsIn(ExpandConstant('{app}'));
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    StopProgramsIn(ExpandConstant('{app}'));
end;
