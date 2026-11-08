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
;
; Every file of the package is installed with its relative path unchanged, so
; the app finds its engine and bundled tools beside course2md-desktop.exe as
; it does in the extracted ZIP. The uninstaller removes only what was installed:
; settings (%APPDATA%\course2md), model caches (%LOCALAPPDATA%\course2md) and
; note libraries live elsewhere and stay.
;
; ChineseSimplified.isl and ChineseTraditional.isl are Inno Setup's user-contributed
; translations (jrsoftware/issrc tag is-6_7_1, Files/Languages/Unofficial).

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
; An upgrade closes whatever runs from the install (the app, its engine and
; tools) through the Restart Manager before replacing files. The app's window
; does not answer the session-end messages the Restart Manager sends first,
; so it is terminated: tasks resume from their checkpoints on the next start.
CloseApplications=force
RestartApplications=no
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\course2md-desktop.exe
UninstallDisplayName=course2md
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}

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
