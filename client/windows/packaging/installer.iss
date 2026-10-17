#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef StageDir
  #error StageDir is required
#endif
#define RepoRoot AddBackslash(SourcePath) + "..\..\.."
[Setup]
AppId={{CC5D3EB0-4865-4A28-B6DB-D520DA29C178}
AppName=AV Relay
AppVersion={#AppVersion}
AppPublisher=AV Relay contributors
AppPublisherURL=https://github.com/salihmarangoz/av_relay
AppSupportURL=https://github.com/salihmarangoz/av_relay/issues
DefaultDirName={localappdata}\Programs\AV Relay
DefaultGroupName=AV Relay
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
AppMutex=Local\AVRelayWindowsReceiver
CloseApplications=yes
RestartApplications=no
SetupIconFile={#RepoRoot}\assets\generated\windows\app.ico
UninstallDisplayIcon={app}\bin\av-relay-windows.exe
LicenseFile={#RepoRoot}\LICENSE
OutputDir=..\dist
OutputBaseFilename=AV-Relay-{#AppVersion}-Windows-x64-Setup
Compression=lzma2/normal
SolidCompression=yes
WizardStyle=modern
DisableProgramGroupPage=yes
ChangesAssociations=no
[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; Flags: unchecked
Name: autostart; Description: "Start AV Relay at login"; Flags: unchecked
[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
[Icons]
Name: "{group}\AV Relay"; Filename: "{app}\bin\av-relay-windows.exe"
Name: "{autodesktop}\AV Relay"; Filename: "{app}\bin\av-relay-windows.exe"; Tasks: desktopicon
[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "AV Relay"; ValueData: """{app}\bin\av-relay-windows.exe"" --at-login"; Tasks: autostart; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "AV Relay"; Flags: uninsdeletevalue
[Run]
Filename: "{app}\bin\av-relay-windows.exe"; Description: "Open AV Relay"; Flags: nowait postinstall skipifsilent
[UninstallRun]
Filename: "{app}\bin\av-relay-windows.exe"; Parameters: "--quit"; Flags: runhidden; RunOnceId: QuitAVRelay
[Code]
procedure InitializeWizard;
var Info: TNewStaticText;
begin
  Info := TNewStaticText.Create(WizardForm);
  Info.Parent := WizardForm.SelectTasksPage;
  Info.Left := 0;
  Info.Top := WizardForm.TasksList.Top + WizardForm.TasksList.Height + ScaleY(8);
  Info.Width := WizardForm.TasksList.Width;
  Info.Height := ScaleY(46);
  Info.WordWrap := True;
  Info.Caption := 'GStreamer is included. Camera output uses OBS Virtual Camera; microphone output uses VB-CABLE. Install those separately if needed.';
end;
