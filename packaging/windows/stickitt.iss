; Windows installer for Stick-itt (Inno Setup 6).
; Built by scripts/package-windows.ps1: ISCC /DAppVersion=<version> packaging\windows\stickitt.iss
; Installs per user (no admin prompt) and leaves notes and settings in %APPDATA%\Stick-itt.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6A2D8C4E-1F3B-4E7A-9C51-5B0E2F7D8A31}
AppName=Stick-itt
AppVersion={#AppVersion}
AppVerName=Stick-itt {#AppVersion}
AppPublisher=shiv122
AppPublisherURL=https://github.com/shiv122/stickitt
AppSupportURL=https://github.com/shiv122/stickitt/issues
SourceDir=..\..
OutputDir=dist
OutputBaseFilename=Stick-itt-{#AppVersion}-windows-setup
DefaultDirName={localappdata}\Programs\Stick-itt
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=assets\stickitt.ico
UninstallDisplayIcon={app}\Stick-itt.exe
UninstallDisplayName=Stick-itt
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; Upgrades close the running copy first (and offer to restart it).
CloseApplications=yes
RestartApplications=yes

[Tasks]
Name: "autostart"; Description: "Start Stick-itt when I sign in"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "target\package\Stick-itt\Stick-itt.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Stick-itt"; Filename: "{app}\Stick-itt.exe"
Name: "{autodesktop}\Stick-itt"; Filename: "{app}\Stick-itt.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Stick-itt"; \
  ValueData: """{app}\Stick-itt.exe"""; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\Stick-itt.exe"; Description: "Open Stick-itt"; Flags: nowait postinstall skipifsilent
