# 星籁剪辑（XingLai Cut）：从 Concat 到本机构的 fork

这份文档记录本机构对 [jub0t/Concat](https://github.com/jub0t/Concat) 0.2.11
的改动，以及按 AGPL-3.0-or-later 分发它时必须带着的东西。

- fork 源码：<https://github.com/A678-beep/concat-fork>
- 上游源码：<https://github.com/jub0t/Concat>（AGPL-3.0-or-later，本 fork 的
  全部文件都还带着它原来的 SPDX 头）
- 本机构的平台：<https://xinglaiai.com>

## 1. 品牌改动

| 位置 | 改动 |
| --- | --- |
| 窗口标题 `crates/concat/ui/app.slint` | `title: "星籁剪辑 XingLai Cut";` |
| 首屏 `crates/concat/ui/start.slint` | 新增 `BrandMark` / `BrandLockup` / `AccountChip` / `AccountDialog`；首屏左上角是星籁的标记与名字，副行写明「基于 Concat（AGPL-3.0）」；右上角是账号入口 |
| 标题条 `crates/concat/ui/title-bar.slint` | 菜单左侧新增 `BrandStrip`（标记 + 星籁剪辑） |
| 关于页 `crates/concat/ui/dialogs/settings.slint` | 版本号下一行写明「基于 Concat（AGPL-3.0-or-later）构建 · 源码：github.com/A678-beep/concat-fork」 |
| 文案 `crates/concat/locales/*.json` | `settings.concat` 全部 27 种语言换成 XingLai Cut；`start.welcome` 换成星籁剪辑 / XingLai Cut |
| 版本行 `crates/concat/src/sysinfo.rs` | `星籁剪辑 XingLai Cut 0.2.11` |
| 启动失败对话框 `crates/concat/src/platform.rs` | 应用名与提示改成星籁剪辑 |
| MCP 服务名 `crates/concat-mcp/src/protocol.rs` | `title` 改成星籁剪辑 XingLai Cut |
| exe 资源 `crates/concat/build.rs` | ProductName / FileDescription 改成星籁剪辑 XingLai Cut；LegalCopyright 同时写上 Concat 作者与 AGPL |
| 图标 `assets/icons/concat.ico` 等 | 换成星籁的标记（青紫渐变圆角方块 + 播放三角），文件名不变，打包脚本不用改 |
| 安装包 `assets/windows/concat.iss` | 应用名、发行者、默认目录、开始菜单、输出文件名（`星籁剪辑-0.2.11-setup.exe`）全部改成星籁；AppId 保持原值，装过旧版的机器是升级而不是装两份 |

版本号仍是 `0.2.11`：这是上游的版本，也写在 Cargo workspace 里；本 fork 的改动
记录在本文档与 git 历史里。

## 2. AGPL 义务

Concat 以 AGPL-3.0-or-later 发布，本 fork 原样继承：

1. **源码公开**：整份源码（含本机构的品牌改动、账号登录、更新源改动）在
   <https://github.com/A678-beep/concat-fork> 公开，默认分支 `main`。
2. **保留声明**：`LICENSE`、`THIRD_PARTY_NOTICES.md`、`LICENSING.md`、
   `TRADEMARK.md` 原样保留；每个源文件的 SPDX 头没有动。
3. **标明修改**：本文件 + git 提交历史（首个提交是
   `chore: import Concat 0.2.11 source (AGPL-3.0)`，之后每个改动一个提交）。
4. **不冒用商标**：Concat 的名字与猫形标记是上游的商标，本 fork 只以
   「基于 Concat 构建」的方式提到它；界面上的星籁标记是本机构自己的。
5. **分发**：安装包里带着 `LICENSE` 与 `THIRD_PARTY_NOTICES.md`（stage 目录
   里就在 exe 旁边），安装卸载信息里写明 Publisher 是「星籁（XingLai）」。
6. **上游工作流**：`.github/workflows/` 移到了 `.github/upstream-workflows/`
   （GitHub 不接受没有 `workflow` 权限的令牌推送工作流文件，而这份 fork 的
   构建在本机做），文件内容一字未改，需要 CI 时移回去即可。

## 3. 自动更新源

**已关闭，并指向本 fork。**

- `crates/concat-host/src/updates.rs`
  - `pub const REPO: &str = "A678-beep/concat-fork";`（原来是 `jub0t/Concat`）
  - `const RELEASES_URL` 跟着改
  - 新增 `pub const SWITCHING: bool = false;`：关着的时候 `fetch_releases()`
    一个网络请求也不发，直接返回空表。Settings > Version 因此显示「还没有
    发行版」，而不是转一圈报错，更不会把上游 Concat 的安装包装回来把界面
    换回去。
- 安装包里的 `AppUpdatesURL` 指向 <https://github.com/A678-beep/concat-fork/releases>。
- 以后要开自动更新：往 fork 发一个带 `manifest.json` 的 release，把
  `SWITCHING` 改成 `true` 重新编译即可（`manifest.json` 的格式见
  `updates.rs` 的模块注释）。

## 4. 星籁 AI 账号登录

- 入口：首屏右上角「用星籁 AI 账号登录」，或登录后的「名字 · 套餐」。
  点开是账号卡（`AccountDialog`，模态）。
- 接口：`POST https://xinglaiai.com/ai-nav/api/auth/login`，body
  `{"phone","password"}`；成功 `{ok, token, expires_at, user{...}}`，
  密码错 401 + `{ok:false, error:"bad_credentials", hint:"..."}`。
- 实现：`crates/concat-host/src/account.rs`（新文件）
  - **直连，不走系统代理**：自己开一条 TCP 连 `xinglaiai.com:443`，
    系统里挂着的 Clash（`127.0.0.1:7897`）对这次的请求没有影响。实测：
    同一个请求经该代理会 `TLS 握手失败`，直连正常。
  - **Windows 上用 OpenSSL（静态链接进 exe）谈 TLS，不用 rustls**：
    平台的 Aliyun WAF 会重置 rustls 与 Schannel 的 ClientHello，只放行
    OpenSSL 形状的（Same-machine matrix in 4.1）。别的平台上没有 DPAPI，
    登录本来也做不了，那里仍走 `net::post`。
  - 接口根地址可以用环境变量 `XINGLAI_API_BASE` 换掉（默认
    `https://xinglaiai.com/ai-nav/api`），方便内网/测试环境。
  - 错误分四类，都带底层原因和 HTTP 状态：地址解析/连不上（网络或代理
    拦了直连）、TLS 握手失败、账号密码不对（401/403，服务端 hint 优先）、
    服务暂时不可用（5xx）。  - 重启时 `account::load()` 解密得开就算已登录，首屏直接显示名字与套餐。
  - 「退出登录」删掉 `account.bin` 并把界面清空；密码从不落盘。
- 失败提示（中文，全部在 `account::refusal` 与 `account_from_json` 里）：
  手机号或密码不对 / 登录太频繁 / 登录失败（HTTP n）/ 连不上星籁账号服务。
- 登录在后台线程做，结果经 `slint::invoke_from_event_loop` 回到界面线程，
  窗口不会卡住。

## 5. 本机构建

构建需要 FFmpeg 8.1 开发库、libclang、预编译的 ONNX Runtime、本地 Skia
包（都是上一轮就装好的），以及一份 OpenSSL 3 的静态库（账号登录用，见
第 4 节）：

```powershell
cd F:\concat\src\src
$env:LIBCLANG_PATH   = 'F:\concat\deps\libclang\runtimes\win-x64\native'
$env:FFMPEG_DIR      = 'F:\concat\deps\ffmpeg-extract\ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-shared-8.1'
$env:ORT_LIB_LOCATION= 'F:\concat\deps\ort128\runtimes\win-x64\native'
$env:ORT_PREFER_DYNAMIC_LINK = '1'
$env:SKIA_BINARIES_URL = 'file://F:/concat/deps/skia-local.tar.gz'
# 静态链接的 OpenSSL 3.6：include/ 与 lib/{libssl,libcrypto}.lib
$env:OPENSSL_DIR     = 'F:\concat\deps\openssl-install'
$env:OPENSSL_STATIC  = '1'
Remove-Item Env:HTTPS_PROXY,Env:HTTP_PROXY -ErrorAction SilentlyContinue
cargo build --release -p concat
```

`openssl-install` 是 `openssl-src 300.6.1+3.6.3` 用 MSVC 静态编出来的
（VC-WIN64A、`no-shared no-module no-tests`），产物直接拿 `install/`
目录。构建它需要 Strawberry Perl 与 NASM，两者都装在 `F:\concat\deps\`
下面；换机器时把 `OPENSSL_DIR` 指到同样的目录即可，exe 本身静态链接，
最终用户不需要 OpenSSL。

安装包（Inno Setup 6.7.3，便携版装在 `F:\concat\innosetup`）：

```powershell
F:\concat\innosetup\ISCC.exe /DVersion=0.2.11 /DArch=x64compatible /DSuffix=x86_64 `
  /DStage=F:\concat\stage\星籁剪辑-0.2.11-windows-x86_64 /DOut=F:\concat\stage `
  F:\concat\src\assets\windows\concat.iss
```

## 6. 已知限制

- 只有 Windows 构建；`account.rs` 在别的平台上明说「DPAPI 只在 Windows 上
  可用」，不会把令牌明文写下去。
- `.msi`（WiX）没做，只有安装包与便携目录。
- 登录的 HTTPS 只支持 `https://`（平台 API 本来就是），不支持代理；要换
  内网/测试地址用 `XINGLAI_API_BASE`，例如
  `set XINGLAI_API_BASE=https://staging.example.com/ai-nav/api`。
- 这台机器上，同一个 `xinglaiai.com` 的 TLS 握手：OpenSSL（Python 3 / Node
  / 本应用）成功；rustls 与 Schannel（curl、.NET、PowerShell）在
  ClientHello 之后被连接重置；经系统里的 Clash（127.0.0.1:7897）转发则
  各种客户端都失败。所以登录这一条路：直连 + OpenSSL。
- 安装包未签名（没有证书），Windows SmartScreen 会提示「未知发布者」。
