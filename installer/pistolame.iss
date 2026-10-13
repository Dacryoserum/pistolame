; Windows installer for Pistolame (Inno Setup 6). Built by scripts/package_release.py:
;   ISCC.exe /DAppVersion=0.1.1 /DSourceDir=<packaged folder> /DOutputDir=<dist> pistolame.iss
; Per-user install (no administrator rights) into %LOCALAPPDATA%\Programs\Pistolame, so the
; app's self-update can replace its own executable.

#ifndef AppVersion
  #error AppVersion must be defined
#endif
#ifndef SourceDir
  #error SourceDir must be defined
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
; Stable identifier: never change it, or upgrades would install side by side.
AppId={{AA4D5745-F4CD-4A63-AF9A-12173B3AE370}
AppName=Pistolame
AppVersion={#AppVersion}
AppVerName=Pistolame {#AppVersion}
AppPublisher=Matthieu Lamy
AppPublisherURL=https://github.com/Dacryoserum/pistolame
AppSupportURL=https://github.com/Dacryoserum/pistolame/issues
AppUpdatesURL=https://github.com/Dacryoserum/pistolame/releases
DefaultDirName={autopf}\Pistolame
DefaultGroupName=Pistolame
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Pistolame-{#AppVersion}-windows-x64-setup
SetupIconFile=..\crates\pistolame-app\assets\app-icon\pistolame.ico
UninstallDisplayIcon={app}\Pistolame.exe
UninstallDisplayName=Pistolame
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
MinVersion=10.0

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Pistolame"; Filename: "{app}\Pistolame.exe"
Name: "{autodesktop}\Pistolame"; Filename: "{app}\Pistolame.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\Pistolame.exe"; Description: "{cm:LaunchProgram,Pistolame}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; A copy left by an interrupted self-update.
Type: files; Name: "{app}\Pistolame.exe.old"
Type: files; Name: "{app}\Pistolame.update"
