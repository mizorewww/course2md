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
; whatever runs from the install instead, before setup or the uninstaller
; touches a file; tasks resume from their checkpoints on the next start.
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
function GetCurrentProcessId: Cardinal;
  external 'GetCurrentProcessId@kernel32.dll stdcall';

// Stops every program running from the install directory (the app, its engine
// and the bundled tools) and waits up to 30 s for them to exit. Only processes
// whose executable lies under Dir are stopped, never a same-named program
// elsewhere. Spared: the uninstaller (unins*.exe), which runs from Dir and
// waits for its copy in TEMP, and this process with all its ancestors.
procedure StopProgramsIn(Dir: String);
var
  Literal, Script: String;
  ResultCode: Integer;
begin
  if not DirExists(Dir) then
    Exit;
  // A PowerShell single-quoted literal doubles its quotes.
  Literal := AddBackslash(Dir);
  StringChangeEx(Literal, '''', '''''', True);
  Script :=
    '$dir = ''' + Literal + '''; ' +
    '$all = @(Get-CimInstance Win32_Process); $spared = @{}; ' +
    '$id = ' + IntToStr(GetCurrentProcessId) + '; ' +
    'while ($id -and -not $spared.ContainsKey([string]$id)) { $spared[[string]$id] = 1; ' +
    '$id = ($all | Where-Object { $_.ProcessId -eq $id } | Select-Object -First 1).ParentProcessId }; ' +
    '$running = { Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and ' +
    '$_.ExecutablePath.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase) -and ' +
    '-not $spared.ContainsKey([string]$_.ProcessId) -and ' +
    '([IO.Path]::GetFileName($_.ExecutablePath) -notlike ''unins*.exe'') } }; ' +
    '& $running | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }; ' +
    '$deadline = (Get-Date).AddSeconds(30); ' +
    'while ((& $running) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 250 }';
  Log('Stopping programs running from ' + Dir);
  if Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
          '-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "' + Script + '"',
          '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    Log('Stopped programs running from the install (exit code ' + IntToStr(ResultCode) + ')')
  else
    Log('Could not run PowerShell to stop programs: ' + SysErrorMessage(ResultCode));
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
