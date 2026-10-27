; New Download Manager installer (Inno Setup 6).
; Build: cargo build --release, then
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\ndm.iss
; Output: installer\out\NDM-Setup-<version>.exe

#define AppName "New Download Manager"
#define AppVersion "1.0.4"
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
; NDM is waited for / asked to exit in [Code] (it hides to the tray, so closing its window doesn't end it)
CloseApplications=no

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
; an update from NDM runs this setup silently: start the new version afterwards
Filename: "{app}\ndm.exe"; Flags: nowait skipifnotsilent

[UninstallDelete]
Type: files; Name: "{app}\ndm.old.exe"
Type: files; Name: "{app}\ndm.*.old"

[Code]
const
  NdmWindow = 'New Download Manager 1.0';
  SYNCHRONIZE = $00100000;

function OpenProcess(Access: DWORD; Inherit: BOOL; Pid: DWORD): THandle; external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(H: THandle; Ms: DWORD): DWORD; external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(H: THandle): BOOL; external 'CloseHandle@kernel32.dll stdcall';

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  I: Integer;
  Exe, Old: String;
  Pid: Integer;
  H: THandle;
begin
  Result := '';
  if WizardSilent() then
  begin
    { started by NDM's "Update now": NDM is closing, give it time to save and exit. /NDMPID= is the copy
      that started it (older copies don't pass it: then any NDM window). Never give up here: a silent
      update that cancels itself looks like nothing happened, and a running ndm.exe is moved aside below }
    Pid := StrToIntDef(ExpandConstant('{param:NDMPID|0}'), 0);
    if Pid <> 0 then
    begin
      H := OpenProcess(SYNCHRONIZE, False, Pid);
      if H <> 0 then
      begin
        WaitForSingleObject(H, 30000);
        CloseHandle(H);
      end;
    end
    else
      for I := 1 to 100 do
      begin
        if FindWindowByWindowName(NdmWindow) = 0 then Break;
        Sleep(200);
      end;
  end
  else
    while FindWindowByWindowName(NdmWindow) <> 0 do
      case MsgBox('New Download Manager is running. Exit it (right-click its tray icon, Exit), then click Retry.',
           mbError, MB_ABORTRETRYIGNORE) of
        IDABORT:
          begin
            Result := 'New Download Manager is still running.';
            Exit;
          end;
        IDIGNORE: Break;
      end;
  { a browser can still hold ndm.exe open as its native messaging host: a running exe can be renamed but
    not replaced, so move it aside (NDM deletes ndm.old.exe at its next start) }
  Exe := ExpandConstant('{app}\ndm.exe');
  if FileExists(Exe) then
  begin
    Old := ExpandConstant('{app}\ndm.old.exe');
    DeleteFile(Old);
    if FileExists(Old) then
      Old := ExpandConstant('{app}\ndm.') + IntToStr(Random(1000000)) + '.old'; { ponytail: stays until uninstall }
    RenameFile(Exe, Old);
  end;
end;
