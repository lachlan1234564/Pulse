; Pulse installer (Inno Setup 6). Built by GitHub Actions:
;   iscc /DAppVersion=0.3.0 installer\pulse.iss
; Installs for the current user only, so no administrator password is needed.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{8E3F2B61-5C4A-4D7E-9B1A-50C0F7A1D3E2}
AppName=Pulse
AppVersion={#AppVersion}
AppVerName=Pulse {#AppVersion}
AppPublisher=Pulse
AppPublisherURL=https://github.com/mercy123xz/Pulse
AppSupportURL=https://github.com/mercy123xz/Pulse/issues
AppUpdatesURL=https://github.com/mercy123xz/Pulse/releases
DefaultDirName={localappdata}\Programs\Pulse
DefaultGroupName=Pulse
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\target\installer
OutputBaseFilename=PulseSetup-{#AppVersion}
SetupIconFile=..\assets\icon\pulse.ico
UninstallDisplayIcon={app}\pulse.exe
UninstallDisplayName=Pulse
WizardStyle=modern
WizardImageFile=..\assets\icon\installer-side.bmp
WizardSmallImageFile=..\assets\icon\installer-small.bmp
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "portuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\target\release\pulse.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Pulse"; Filename: "{app}\pulse.exe"
Name: "{autodesktop}\Pulse"; Filename: "{app}\pulse.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\pulse.exe"; Description: "{cm:LaunchProgram,Pulse}"; Flags: nowait postinstall skipifsilent
; After an update from inside Pulse (a silent install), start the new version.
Filename: "{app}\pulse.exe"; Flags: nowait runasoriginaluser; Check: WizardSilent

; Uninstalling asks what else to delete from %APPDATA%\pulse-player (see [Code]).
; By default only temporary files go: your library and downloaded songs are kept, so
; reinstalling picks up where you left off. A silent uninstall keeps everything.

[CustomMessages]
RemoveTitle=Uninstall Pulse
RemoveIntro=Pulse is being removed from this PC. Do you also want to delete these?
RemoveCache=Temporary files (cover art, downloaded tools, logs)
RemoveSongs=Downloaded songs in Pulse's own folder
RemoveSongsNote=A downloads folder that you picked yourself is never deleted.
RemoveLibrary=My library: likes, playlists, profile, themes, backups and settings
RemoveLibraryNote=Leave this off if you might install Pulse again.
RemoveContinue=Continue

[Code]
var
  DeleteCache, DeleteSongs, DeleteLibrary: Boolean;

{ %APPDATA%\pulse-player, or a folder or file in it. }
function PulseData(Sub: String): String;
begin
  Result := ExpandConstant('{userappdata}\pulse-player');
  if Sub <> '' then
    Result := Result + '\' + Sub;
end;

function AddNote(Form: TSetupForm; Text: String; Top: Integer): TNewStaticText;
begin
  Result := TNewStaticText.Create(Form);
  Result.Parent := Form;
  Result.Left := ScaleX(36);
  Result.Top := Top;
  Result.Width := Form.ClientWidth - ScaleX(52);
  Result.WordWrap := True;
  Result.Font.Color := clGrayText;
  Result.Caption := Text;
end;

function AddBox(Form: TSetupForm; Text: String; Top: Integer; On: Boolean): TNewCheckBox;
begin
  Result := TNewCheckBox.Create(Form);
  Result.Parent := Form;
  Result.Left := ScaleX(16);
  Result.Top := Top;
  Result.Width := Form.ClientWidth - ScaleX(32);
  Result.Height := ScaleY(20);
  Result.Caption := Text;
  Result.Checked := On;
end;

{ The plain fallback window, if Pulse's own one can't open. }
procedure AskInPlainWindow();
var
  Form: TSetupForm;
  Intro, Note: TNewStaticText;
  CacheBox, SongsBox, LibraryBox: TNewCheckBox;
  Button: TNewButton;
  Top: Integer;
begin
  Form := CreateCustomForm(ScaleX(460), ScaleY(250), False, False);
  try
    Form.Caption := CustomMessage('RemoveTitle');
    Intro := TNewStaticText.Create(Form);
    Intro.Parent := Form;
    Intro.Left := ScaleX(16);
    Intro.Top := ScaleY(16);
    Intro.Width := Form.ClientWidth - ScaleX(32);
    Intro.WordWrap := True;
    Intro.Caption := CustomMessage('RemoveIntro');
    Top := Intro.Top + Intro.Height + ScaleY(14);

    CacheBox := AddBox(Form, CustomMessage('RemoveCache'), Top, True);
    Top := Top + ScaleY(28);
    SongsBox := AddBox(Form, CustomMessage('RemoveSongs'), Top, False);
    Note := AddNote(Form, CustomMessage('RemoveSongsNote'), Top + ScaleY(20));
    Top := Note.Top + Note.Height + ScaleY(10);
    LibraryBox := AddBox(Form, CustomMessage('RemoveLibrary'), Top, False);
    Note := AddNote(Form, CustomMessage('RemoveLibraryNote'), Top + ScaleY(20));

    Button := TNewButton.Create(Form);
    Button.Parent := Form;
    Button.Width := ScaleX(96);
    Button.Height := ScaleY(26);
    Button.Left := Form.ClientWidth - Button.Width - ScaleX(16);
    Button.Top := Form.ClientHeight - Button.Height - ScaleY(14);
    Button.Caption := CustomMessage('RemoveContinue');
    Button.ModalResult := mrOk;
    Button.Default := True;
    Form.ActiveControl := Button;

    { Closing the window with X keeps everything. }
    if Form.ShowModal() = mrOk then
    begin
      DeleteCache := CacheBox.Checked;
      DeleteSongs := SongsBox.Checked;
      DeleteLibrary := LibraryBox.Checked;
    end;
  finally
    Form.Free();
  end;
end;

{ Asked once Windows has confirmed the uninstall, so there's no second "Are you sure?".
  Pulse asks in its own window and answers with its exit code: 16, plus 1 for temporary
  files, 2 for downloaded songs and 4 for the library. }
procedure AskWhatToDelete();
var
  Code: Integer;
begin
  if Exec(ExpandConstant('{app}\pulse.exe'), '--uninstall-options', '', SW_SHOW, ewWaitUntilTerminated, Code)
    and (Code >= 16) and (Code <= 23) then
  begin
    DeleteCache := (Code and 1) <> 0;
    DeleteSongs := (Code and 2) <> 0;
    DeleteLibrary := (Code and 4) <> 0;
  end
  else
    AskInPlainWindow();
end;

procedure DeleteFolder(Sub: String);
begin
  DelTree(PulseData(Sub), True, True, True);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usUninstall) and not UninstallSilent and DirExists(PulseData('')) then
    AskWhatToDelete();
  if CurUninstallStep <> usPostUninstall then
    Exit;
  if DeleteCache and DeleteSongs and DeleteLibrary then
  begin
    DeleteFolder('');
    Exit;
  end;
  if DeleteCache then
  begin
    DeleteFolder('cache');
    DeleteFolder('tools');
    DeleteFolder('logs');
    DeleteFolder('crash-reports');
  end;
  if DeleteSongs then
    DeleteFolder('offline');
  if DeleteLibrary then
  begin
    DeleteFolder('data');
    DeleteFolder('covers');
    DeleteFolder('themes');
    DeleteFolder('backups');
    DeleteFile(PulseData('library.ron'));
    DeleteFile(PulseData('account.json'));
    DeleteFile(PulseData('no-signup'));
    DeleteFile(PulseData('software-rendering'));
  end;
end;
