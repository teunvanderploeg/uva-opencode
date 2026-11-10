#define AppVersion "0.4.0"
[Setup]
AppId=UvAOpenCodeUnofficial
AppName=UvA OpenCode (unofficial)
AppVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\UvAOpenCode
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=uva-opencode-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
ChangesEnvironment=yes
[Files]
Source: "..\dist\native\uva-opencode\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs
[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Flags: preservestringtype
[Icons]
Name: "{userprograms}\UvA OpenCode Login"; Filename: "{app}\uva-opencode.exe"; Parameters: "--login"
