; New Download Manager installer (Inno Setup 6).
; Build: cargo build --release, then
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\ndm.iss
; Output: installer\out\NDM-Setup-<version>.exe

#define AppName "New Download Manager"
#define AppVersion "1.0.0"
#define AppUrl "https://github.com/MoeJoe37/New-Download-Manager"

[Setup]
AppId={{6E2B9C47-1F3A-4C8D-9B57-0D3E8A1F4C21}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=MoeJoe37
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
UninstallDisplayIcon={app}\ndm.exe
LicenseFile=..\LICENSE
SetupIconFile=..\assets\ndm.ico
OutputDir=out
OutputBaseFilename=NDM-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; NDM keeps everything per user (HKCU, %APPDATA%), so it installs without admin rights unless asked to
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; a running NDM is closed for the update / uninstall
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "autostart"; Description: "Start New Download Manager with Windows"; GroupDescription: "Startup:"; Flags: unchecked

[Files]
Source: "..\target\release\ndm.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\Languages\*.lng"; DestDir: "{app}\Languages"; Flags: ignoreversion
Source: "..\help\*.htm"; DestDir: "{app}\help"; Flags: ignoreversion
Source: "..\extension\*"; DestDir: "{app}\extension"; Flags: ignoreversion recursesubdirs
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\ndm.exe"
Name: "{group}\Browser extension folder"; Filename: "{app}\extension"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\ndm.exe"; Tasks: desktopicon

[Registry]
; same value NDM writes for Options -> General -> "Launch New Download Manager on startup"
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "NewDownloadManager"; ValueData: """{app}\ndm.exe"""; Flags: uninsdeletevalue; Tasks: autostart
; the browser extension's native messaging host, registered by NDM at start; removed with NDM
Root: HKCU; Subkey: "Software\Google\Chrome\NativeMessagingHosts\com.ndm.host"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Microsoft\Edge\NativeMessagingHosts\com.ndm.host"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Chromium\NativeMessagingHosts\com.ndm.host"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.ndm.host"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Mozilla\NativeMessagingHosts\com.ndm.host"; Flags: uninsdeletekey dontcreatekey

[Run]
Filename: "{app}\ndm.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
