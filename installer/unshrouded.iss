; Unshrouded installer (Inno Setup 6). Build from the repository root after `cargo build --release --workspace` in planner/:
;   ISCC.exe installer\unshrouded.iss
; Installs for the current user only (no admin) into %LOCALAPPDATA%\Programs\Unshrouded:
;   Unshrouded.exe            the planner and live meter (planner-desktop.exe)
;   ens_overlay.dll           the in-game meter layer; the planner copies it into Combat Logger when the meter is on
;   Combat Logger\ens_hook.dll the combat logger; it writes its logs into this folder, so the folder must be writable
; Saved builds and settings live in the app's data folder (identifier com.enshroudedbuild.planner) and survive updates
; and uninstalls.

#define AppName "Unshrouded"
#define AppVersion "0.1.0"
#define Release "..\planner\target\release"

[Setup]
AppId={{6C1E7B54-2D0F-4E8B-9A71-3F5C2B8D9E40}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Destrayon
AppPublisherURL=https://github.com/Destrayon/Unshrouded
AppComments=Build planner and DPS meter for Enshrouded
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\planner\dist
OutputBaseFilename={#AppName}-Setup-{#AppVersion}
SetupIconFile=..\planner\crates\ens-desktop\icons\icon.ico
UninstallDisplayIcon={app}\{#AppName}.exe
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Dirs]
Name: "{app}\Combat Logger"

[Files]
Source: "{#Release}\planner-desktop.exe"; DestDir: "{app}"; DestName: "{#AppName}.exe"; Flags: ignoreversion
Source: "{#Release}\ens_overlay.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Release}\ens_hook.dll"; DestDir: "{app}\Combat Logger"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppName}.exe"; Comment: "Build planner and DPS meter for Enshrouded"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppName}.exe"; Comment: "Build planner and DPS meter for Enshrouded"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppName}.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; written by the planner when the in-game meter is switched on; the combat logs (combat-*.csv) are the player's and stay
Type: files; Name: "{app}\Combat Logger\VK_LAYER_ENS_meter.json"
Type: files; Name: "{app}\Combat Logger\ens_overlay.dll"
Type: files; Name: "{app}\Combat Logger\*.log"
Type: files; Name: "{app}\Combat Logger\overlay.bin"

[Code]
const
  LayerKey = 'Software\Khronos\Vulkan\ImplicitLayers';

{ The planner registers the in-game meter as a per-user Vulkan layer (a value named by its manifest's path); remove
  every registration that points into this install so the game stops loading it. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Names: TArrayOfString;
  I: Integer;
  Dir: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    Dir := Lowercase(AddBackslash(ExpandConstant('{app}')));
    if RegGetValueNames(HKCU, LayerKey, Names) then
      for I := 0 to GetArrayLength(Names) - 1 do
        if Pos(Dir, Lowercase(Names[I])) = 1 then
          RegDeleteValue(HKCU, LayerKey, Names[I]);
  end;
end;
