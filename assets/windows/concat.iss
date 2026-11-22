; 星籁剪辑 (XingLai Cut) 的 Windows 安装包。
; 基于 Concat（AGPL-3.0-or-later）构建；源码见
; https://github.com/A678-beep/concat-fork 。
;
; Built by build-app.yml with Inno Setup from the staged folder the .msi
; is also made of, so the two ship the same files; this is the one a
; person double-clicks, and it puts Concat in the Start menu and can take
; it out again. Everything it needs is handed in on the command line:
;
;   iscc /DVersion=0.2.11 /DArch=x64compatible /DSuffix=x86_64 ^
;        /DStage=C:\...\stage\Concat-0.2.11-windows-x86_64 /DOut=C:\...\stage ^
;        assets\windows\concat.iss
;
; Arch is Inno's own word for the machine: x64compatible for the x86_64
; build (which also installs on ARM PCs, under emulation), arm64 for the
; native one. Suffix is the bundle's word for it, which names the file.

#ifndef Version
  #error Version is required
#endif
#ifndef Arch
  #error Arch is required: x64compatible or arm64
#endif
#ifndef Suffix
  #error Suffix is required: x86_64 or aarch64
#endif
#ifndef Stage
  #error Stage is required: the staged folder to install
#endif
#ifndef Out
  #define Out "."
#endif

[Setup]
; One id for the life of the product, so an install over an older one is
; an upgrade and not a second copy.
AppId={{7B1E5C3A-3B9E-4F0B-9C6D-2F1D0C0A0C47}
AppName=星籁剪辑 XingLai Cut
AppVersion={#Version}
AppVerName=星籁剪辑 XingLai Cut {#Version}
AppPublisher=星籁（XingLai）
AppPublisherURL=https://xinglaiai.com
AppSupportURL=https://github.com/A678-beep/concat-fork/issues
AppUpdatesURL=https://github.com/A678-beep/concat-fork/releases
DefaultDirName={autopf}\星籁剪辑
DefaultGroupName=星籁剪辑
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir={#Out}
OutputBaseFilename=星籁剪辑-{#Version}-setup
SetupIconFile=..\icons\concat.ico
UninstallDisplayIcon={app}\concat.ico
UninstallDisplayName=星籁剪辑 XingLai Cut
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed={#Arch}
ArchitecturesInstallIn64BitMode={#Arch}
; For the user alone unless they ask for the machine: no prompt for an
; administrator to install a video editor into one's own account.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; Follow Windows' display language when the setup speaks it, and ask only
; when it does not.
ShowLanguageDialog=auto

[Languages]
; Every language the app ships. Inno Setup carries the official
; translations. The ones it keeps as unofficial - Chinese, Persian,
; Croatian, Bengali, Hindi, Indonesian, Urdu, Vietnamese - and the ones
; made official lately - Korean, Arabic and Thai, which an older compiler
; lacks - are vendored in languages/ from the Inno Setup 6.7.1 tree
; (https://github.com/jrsoftware/issrc/tree/is-6_7_1/Files/Languages),
; verbatim, so the setup offers them whatever the compiler on the build
; machine ships (#277). Some are in an older format, which the compiler
; still reads with the newer messages in English and a warning for each:
; Traditional Chinese 6.1, Urdu 6.1, Indonesian 6.4, and Bengali and Hindi
; 5.5.3, whose obsolete title and copyright font sizes it ignores.
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "italian"; MessagesFile: "compiler:Languages\Italian.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "turkish"; MessagesFile: "compiler:Languages\Turkish.isl"
Name: "chinesesimplified"; MessagesFile: "languages\ChineseSimplified.isl"
Name: "chinesetraditional"; MessagesFile: "languages\ChineseTraditional.isl"
Name: "korean"; MessagesFile: "languages\Korean.isl"
Name: "croatian"; MessagesFile: "languages\Croatian.isl"
Name: "farsi"; MessagesFile: "languages\Farsi.isl"
Name: "dutch"; MessagesFile: "compiler:Languages\Dutch.isl"
Name: "hebrew"; MessagesFile: "compiler:Languages\Hebrew.isl"
Name: "polish"; MessagesFile: "compiler:Languages\Polish.isl"
Name: "ukrainian"; MessagesFile: "compiler:Languages\Ukrainian.isl"
Name: "arabic"; MessagesFile: "languages\Arabic.isl"
Name: "thai"; MessagesFile: "languages\Thai.isl"
Name: "bengali"; MessagesFile: "languages\Bengali.islu"
Name: "hindi"; MessagesFile: "languages\Hindi.islu"
Name: "indonesian"; MessagesFile: "languages\Indonesian.isl"
Name: "urdu"; MessagesFile: "languages\Urdu.isl"
Name: "vietnamese"; MessagesFile: "languages\Vietnamese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Stage}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\icons\concat.ico"; DestDir: "{app}"; Flags: ignoreversion
; AGPL/LGPL 义务入口：许可证全文、第三方声明、FFmpeg 对应源码要约与 AI 插件/模型清单。
; 已 staged 的构建不需要这些行（{#Stage}\* 会带上），直接跑本 .iss 时兜底。
Source: "..\..\LICENSES\*"; DestDir: "{app}\LICENSES"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
Source: "..\..\SOURCE_OFFER.md"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\..\AI_PLUGINS_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
; 自动字幕的中文语音模型：一键安装脚本 + 使用说明。488 MB 的 ggml-small.bin
; 不随安装包分发；脚本优先复制 {app}\模型\ggml-small.bin（离线），
; 否则按系统代理 → HuggingFace → hf-mirror → GitHub 镜像下载，校验 SHA-256 后安装。
Source: "..\..\assets\windows\字幕模型\*"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\星籁剪辑"; Filename: "{app}\concat.exe"; IconFilename: "{app}\concat.ico"
; The way in when a graphics driver draws the window as coloured squares:
; it turns off Settings > General > Draw on the GPU, which that window hides.
Name: "{autoprograms}\星籁剪辑（安全图形）"; Filename: "{app}\concat.exe"; Parameters: "--software"; IconFilename: "{app}\concat.ico"
Name: "{autodesktop}\星籁剪辑"; Filename: "{app}\concat.exe"; IconFilename: "{app}\concat.ico"; Tasks: desktopicon
; 中文自动字幕：给第一次用的商家一个能看懂、能直接双击的入口（模型需单独装一次）。
Name: "{autoprograms}\星籁剪辑 · 安装中文语音模型（自动字幕）"; Filename: "{app}\安装中文语音模型.cmd"; IconFilename: "{app}\concat.ico"; WorkingDir: "{app}"

[Run]
; 0.25.0: a first launch right after an install took 12 s on the boss's
; machine - Windows scanning the freshly written 188 MB binary (and the
; libraries it imports) for the first time, before `main` was reached.
; That scan happens here instead, where a few seconds of install time are
; expected: `--version` loads the executable and everything it links and
; exits before the log, the GPU and a window. The person's first launch is
; the second time Windows has seen these files.
Filename: "{app}\concat.exe"; Parameters: "--version"; Flags: runhidden waituntilterminated; StatusMsg: "Preparing the program files"
Filename: "{app}\concat.exe"; Description: "{cm:LaunchProgram,星籁剪辑}"; Flags: nowait postinstall skipifsilent
