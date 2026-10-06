; A per-user installer. AppId stays stable across upgrades; application data is
; deliberately outside this file manifest and is never removed on uninstall.
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef AppNumericVersion
  #error AppNumericVersion is required
#endif
#ifndef PackageRoot
  #error PackageRoot is required
#endif
#ifndef OutputRoot
  #error OutputRoot is required
#endif

[Setup]
AppId={{760E17BA-7188-4C02-9467-590D1092B47D}
AppName=TerminalCanvas
AppVersion={#AppVersion}
VersionInfoVersion={#AppNumericVersion}
VersionInfoProductVersion={#AppNumericVersion}
VersionInfoProductTextVersion={#AppVersion}
AppPublisher=TerminalCanvas
AppPublisherURL=https://github.com/MauroProto/terminal-canvas
AppSupportURL=https://github.com/MauroProto/terminal-canvas/issues
AppUpdatesURL=https://github.com/MauroProto/terminal-canvas/releases
DefaultDirName={localappdata}\Programs\TerminalCanvas
DefaultGroupName=TerminalCanvas
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
MinVersion=10.0.17763
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputRoot}
OutputBaseFilename=TerminalCanvas-{#AppVersion}-windows-x86_64-setup
LicenseFile={#PackageRoot}\LICENSE
UninstallDisplayIcon={app}\mi-terminal.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=no
RestartApplications=no
#ifdef SignedBuild
SignTool=terminalcanvas
SignedUninstaller=yes
#endif

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#PackageRoot}\mi-terminal.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\tc-memory.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\tc-memory-mcp.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageRoot}\PORTABLE.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\TerminalCanvas"; Filename: "{app}\mi-terminal.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\TerminalCanvas"; Filename: "{app}\mi-terminal.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\mi-terminal.exe"; Description: "Launch TerminalCanvas"; Flags: nowait postinstall skipifsilent
