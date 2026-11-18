; ValoLights installer (Inno Setup 6.4+). Built by build\installer.ps1:
;   ISCC /DAppVersion=0.2.0 /DPayload=<staged app folder> build\installer.iss
;
; Small on purpose: it carries ValoLights itself (app, speech helper, assets, frame interpolator, the
; Visual C++ and CUDA runtimes it links against) and downloads the big third-party parts during setup from
; their official, version-pinned releases, each checked against its SHA-256:
;   ffmpeg 9.0.2 essentials (gyan.dev, via GyanD/codexffmpeg on GitHub)  -> bin\ffmpeg.exe, bin\ffprobe.exe
;   libmpv 20261111 (shinchiro/mpv-winbuild-cmake on GitHub)              -> bin\libmpv-2.dll
;   cuBLAS 13.4.0 (NVIDIA, the nvidia-cublas wheel on PyPI)               -> cublas64_13.dll, cublasLt64_13.dll
; These are byte-identical to the files the app is built and tested with. Keep the URLs and checksums in step with
; toolsetch_binaries.ps1, which downloads the same pinned files for development builds. Already installed copies with the
; right checksum are kept, so updating ValoLights doesn't download them again.
;
; Per-user install into %LOCALAPPDATA%\Programs\ValoLights: no administrator prompt, and the app can write
; its data\ folder (projects, settings, models) next to itself as it expects.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Payload
  #define Payload "..\build\out\installer-payload"
#endif

#define FfmpegUrl    "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip"
#define FfmpegSha    "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba"
#define FfmpegExeSha "3256173f3f8bffd7df12227c68adf68025edb1832273a9530688a7bb1ed8edec"
#define FfprobeSha   "f0d36ecbbdd3bcfac3efa078c96c7271c2e68b3810595552ac3b7f17e9a65c52"
#define MpvUrl       "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261111/mpv-dev-x86_64-20261111-git-bcde6b93cc.7z"
#define MpvSha       "273c079b11ea5575822f972756f4c9a71149a5c10130353abeb8586738938452"
#define MpvDllSha    "88504ee5560942a955405e99dfb5971368e4cf851321f36565bf2a667271db1b"
#define CublasUrl    "https://files.pythonhosted.org/packages/d6/97/a7ac03495c1ca3c250f12f094dec46e945e38c06f24f3b11c168894a37d1/nvidia_cublas-13.4.0.1-py3-none-win_amd64.whl"
#define CublasSha    "37a142e2643c928f498f12f4f61c2ecd1c6c6c9608e16f6a4d45ec8d5d057733"
#define CublasDllSha "537d278122e2621d7202653cf873a9b8b872fac37122cb9d36246ded33c53e03"
#define CublasLtSha  "b37233a1e3516a41d27dcea49595fd1cbaff979b65c80da69935cb59d0d23f5b"

[Setup]
AppId={{6F1C2B7E-3D4A-4B8E-9C61-7A5E2D9B0F14}
AppName=ValoLights
AppVersion={#AppVersion}
AppVerName=ValoLights {#AppVersion}
AppPublisher=ValoLights
DefaultDirName={localappdata}\Programs\ValoLights
DisableProgramGroupPage=yes
DisableWelcomePage=no
DefaultGroupName=ValoLights
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\dist
OutputBaseFilename=ValoLights-Setup-{#AppVersion}
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\ValoLights.exe
UninstallDisplayName=ValoLights
; the app's look: always dark, no bevel lines, styled title bar, the page colour of the app and its artwork
; (build\installer_art.py draws build\installer\*.png from the app's theme)
WizardStyle=modern dark hidebevels includetitlebar
WizardBackColor=#141415
WizardImageFile=installer\wizard-164x314.png,installer\wizard-192x386.png,installer\wizard-246x459.png,installer\wizard-273x556.png,installer\wizard-328x604.png,installer\wizard-355x700.png,installer\wizard-410x797.png
WizardSmallImageFile=installer\small-55x55.png,installer\small-64x68.png,installer\small-69x69.png,installer\small-83x80.png,installer\small-92x97.png,installer\small-110x106.png,installer\small-119x123.png
Compression=lzma2/ultra64
SolidCompression=yes
LZMAUseSeparateProcess=yes
ArchiveExtraction=full
; the downloads, unpacked, plus room for the speech model the app fetches on first use
ExtraDiskSpaceRequired=1300000000
CloseApplications=yes
RestartApplications=no

[Messages]
WelcomeLabel2=This will install ValoLights {#AppVersion} on your computer.%n%nDuring setup about 550 MB of video and GPU libraries (ffmpeg, mpv and NVIDIA cuBLAS) are downloaded from their official sources. Nothing about you is sent anywhere.

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"

[Files]
Source: "{#Payload}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\ValoLights"; Filename: "{app}\ValoLights.exe"
Name: "{autodesktop}\ValoLights"; Filename: "{app}\ValoLights.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\ValoLights.exe"; Description: "Start ValoLights"; Flags: nowait postinstall skipifsilent
; an update started from inside the app (/SILENT /RELAUNCH=1): reopen it when done
Filename: "{app}\ValoLights.exe"; Flags: nowait; Check: RelaunchAfterUpdate

[UninstallDelete]
; unpacked by the setup itself, so not in its file list
Type: files; Name: "{app}\bin\ffmpeg.exe"
Type: files; Name: "{app}\bin\ffprobe.exe"
Type: files; Name: "{app}\bin\libmpv-2.dll"
Type: files; Name: "{app}\cublas64_13.dll"
Type: files; Name: "{app}\cublasLt64_13.dll"
Type: files; Name: "{app}\licenses\NVIDIA-cuBLAS-License.txt"
Type: files; Name: "{app}\bin\FFMPEG_LICENSE.txt"

[Code]
var
  DownloadPage: TDownloadWizardPage;
  NeedFfmpeg, NeedMpv, NeedCublas: Boolean;

{ the app runs the installer with /RELAUNCH=1 when it updates itself }
function RelaunchAfterUpdate: Boolean;
begin
  Result := WizardSilent and (ExpandConstant('{param:RELAUNCH|0}') = '1');
end;

function OnDownloadProgress(const Url, FileName: String; const Progress, ProgressMax: Int64): Boolean;
begin
  Result := True;
end;

{ True when the file exists and has the expected SHA-256 }
function HasFile(const Path, Sha: String): Boolean;
begin
  Result := FileExists(Path);
  if Result then
    try
      Result := (Lowercase(GetSHA256OfFile(Path)) = Sha);
    except
      Result := False;
    end;
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
  if not FileExists(ExpandConstant('{sys}\nvcuda.dll')) then
    SuppressibleMsgBox('No NVIDIA graphics driver was found on this PC.' + #13#10#13#10 +
      'ValoLights still works, but analysis, subtitles and exports will be slower, and the AI ' +
      'super smooth modes may not be available (the Faster one works everywhere).',
      mbInformation, MB_OK, IDOK);
end;

procedure InitializeWizard;
begin
  { theme colours (crates/vl-app/src/theme.rs), TColor is $BBGGRR: accent #f4a6c4, text #ededed,
    secondary #a1a1a6, deep #0f0f10 for the header like the app's top bar }
  { the dark style recolours fonts unless they're left out of its styling }
  WizardForm.WelcomeLabel1.StyleElements := [seClient, seBorder];
  WizardForm.WelcomeLabel1.Font.Color := $C4A6F4;
  WizardForm.FinishedHeadingLabel.StyleElements := [seClient, seBorder];
  WizardForm.FinishedHeadingLabel.Font.Color := $C4A6F4;
  WizardForm.PageNameLabel.Font.Color := $EDEDED;
  WizardForm.PageDescriptionLabel.Font.Color := $A6A1A1;
  WizardForm.MainPanel.StyleElements := [seFont, seBorder];
  WizardForm.MainPanel.Color := $100F0F;
  DownloadPage := CreateDownloadPage('Downloading video and GPU libraries',
    'ValoLights needs ffmpeg, mpv and NVIDIA cuBLAS. They are downloaded from their official releases and checked.',
    @OnDownloadProgress);
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  App: String;
begin
  Result := True;
  if CurPageID <> wpReady then
    exit;
  App := WizardDirValue;
  NeedFfmpeg := not (HasFile(App + '\bin\ffmpeg.exe', '{#FfmpegExeSha}') and HasFile(App + '\bin\ffprobe.exe', '{#FfprobeSha}'));
  NeedMpv := not HasFile(App + '\bin\libmpv-2.dll', '{#MpvDllSha}');
  NeedCublas := not (HasFile(App + '\cublas64_13.dll', '{#CublasDllSha}') and HasFile(App + '\cublasLt64_13.dll', '{#CublasLtSha}'));
  if not (NeedFfmpeg or NeedMpv or NeedCublas) then
    exit;
  DownloadPage.Clear;
  if NeedFfmpeg then
    DownloadPage.Add('{#FfmpegUrl}', 'ffmpeg.zip', '{#FfmpegSha}');
  if NeedMpv then
    DownloadPage.Add('{#MpvUrl}', 'mpv.7z', '{#MpvSha}');
  if NeedCublas then
    DownloadPage.Add('{#CublasUrl}', 'cublas.zip', '{#CublasSha}');
  DownloadPage.Show;
  try
    try
      DownloadPage.Download;
    except
      if DownloadPage.AbortedByUser then
        Log('Download aborted by user.')
      else
        SuppressibleMsgBox('A download failed: ' + AddPeriod(GetExceptionMessage) + #13#10#13#10 +
          'Check your internet connection and try again.', mbCriticalError, MB_OK, IDOK);
      Result := False;
    end;
  finally
    DownloadPage.Hide;
  end;
end;

{ Copy one unpacked file into the app folder, checking it is the expected one }
procedure Place(const Src, Dst, Sha: String);
begin
  if not FileCopy(Src, Dst, False) then
    RaiseException('Could not write ' + Dst);
  if (Sha <> '') and not HasFile(Dst, Sha) then
    RaiseException('Unexpected contents in ' + ExtractFileName(Dst));
end;

procedure Unpack(const Archive, Dir: String);
begin
  WizardForm.StatusLabel.Caption := 'Unpacking ' + ExtractFileName(Archive) + '...';
  ExtractArchive(ExpandConstant('{tmp}\') + Archive, Dir, '', True, nil);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  App, X: String;
begin
  if CurStep <> ssPostInstall then
    exit;
  App := ExpandConstant('{app}');
  X := ExpandConstant('{tmp}\x');
  ForceDirectories(App + '\bin');
  ForceDirectories(App + '\licenses');
  if NeedFfmpeg then begin
    Unpack('ffmpeg.zip', X);
    Place(X + '\ffmpeg-9.0.2-essentials_build\bin\ffmpeg.exe', App + '\bin\ffmpeg.exe', '{#FfmpegExeSha}');
    Place(X + '\ffmpeg-9.0.2-essentials_build\bin\ffprobe.exe', App + '\bin\ffprobe.exe', '{#FfprobeSha}');
    Place(X + '\ffmpeg-9.0.2-essentials_build\LICENSE', App + '\bin\FFMPEG_LICENSE.txt', '');
  end;
  if NeedMpv then begin
    Unpack('mpv.7z', X);
    Place(X + '\libmpv-2.dll', App + '\bin\libmpv-2.dll', '{#MpvDllSha}');
  end;
  if NeedCublas then begin
    Unpack('cublas.zip', X);
    Place(X + '\nvidia\cu13\bin\x86_64\cublas64_13.dll', App + '\cublas64_13.dll', '{#CublasDllSha}');
    Place(X + '\nvidia\cu13\bin\x86_64\cublasLt64_13.dll', App + '\cublasLt64_13.dll', '{#CublasLtSha}');
    Place(X + '\nvidia_cublas-13.4.0.1.dist-info\licenses\License.txt', App + '\licenses\NVIDIA-cuBLAS-License.txt', '');
  end;
  DelTree(X, True, True, True);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Data: String;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  Data := ExpandConstant('{app}\data');
  if DirExists(Data) then
    if SuppressibleMsgBox('Also delete your ValoLights projects, settings, exported videos and downloaded models?' + #13#10#13#10 +
        Data, mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
      DelTree(Data, True, True, True);
  RemoveDir(ExpandConstant('{app}'));
end;
