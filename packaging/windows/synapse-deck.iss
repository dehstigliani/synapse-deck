; Instalador do Synapse Deck para Windows (Inno Setup).
;
; Escolhido no lugar do MSI porque o MSI exige versão numérica de três partes e
; recusaria "1.0.0-alpha.2". O Inno aceita a versão como ela é.
;
; A versão chega pela linha de comando:
;   iscc /DAppVersion=1.0.0-alpha.2 /DBinary=caminho\synapse-deck.exe synapse-deck.iss

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef Binary
  #define Binary "synapse-deck.exe"
#endif

#define AppName "Synapse Deck"
#define AppPublisher "André Stigliani"
#define AppUrl "https://github.com/dehstigliani/synapse-deck"

[Setup]
AppId={{7B1E4C2A-5D3F-4A88-9C21-6F0B2E7A9D14}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Instalação por usuário: não pede elevação, e é o que faz sentido para uma
; ferramenta pessoal. O atualizador embutido também precisa disso — ele troca o
; próprio executável, o que exigiria privilégio se ficasse em Arquivos de Programas.
PrivilegesRequired=lowest
OutputBaseFilename=synapse-deck-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#AppName} {#AppVersion}
; O ícone do próprio instalador e o que aparece em Programas e Recursos.
; Usa favicon.ico (formato híbrido BMP+PNG que o Inno lê sem ressalva);
; o executável usa app.ico, que tem arte própria para 16 e 24 px.
SetupIconFile=..\..\web\assets\favicon.ico
UninstallDisplayIcon={app}\synapse-deck.exe
LicenseFile=..\..\LICENSE

[Languages]
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Criar atalho na área de trabalho"; GroupDescription: "Atalhos:"; Flags: unchecked

[Files]
Source: "{#Binary}"; DestDir: "{app}"; DestName: "synapse-deck.exe"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\CHANGELOG.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\synapse-deck.exe"
Name: "{group}\Desinstalar {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\synapse-deck.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\synapse-deck.exe"; Description: "Abrir o {#AppName} agora"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; O atualizador embutido deixa o binário anterior como .old; a desinstalação leva junto.
Type: files; Name: "{app}\synapse-deck.old"
Type: files; Name: "{app}\synapse-deck.new"
