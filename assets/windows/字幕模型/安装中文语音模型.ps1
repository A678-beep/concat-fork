# 星籁剪辑 · 中文语音模型一键安装
# 模型：ggml-small.bin（whisper.cpp small，多语言，487,601,967 B，中文效果好）
#
# 安装位置：
#   便携版（exe 同目录有 portable\ 文件夹）→ <安装目录>\portable\whisper-models
#   安装版                                → %LOCALAPPDATA%\app.concat.editor\whisper-models
#
# 行为：
#   1. 已装好（大小 + SHA-256 一致）→ 直接退出；
#   2. <安装目录>\模型\ggml-small.bin 存在 → 复制，离线安装；
#   3. 否则依次尝试 系统代理 → HuggingFace → hf-mirror → GitHub 镜像 下载；
#   4. 校验 SHA-256 后放到模型目录。

$ErrorActionPreference = 'Stop'
$name  = 'ggml-small.bin'
$bytes = [int64]487601967
$sha   = '1BE3A9B2063867B937E64E2EC7483364A79917E157FA98C5D94B5C1FFFE987B'
$root  = Split-Path -Parent $MyInvocation.MyCommand.Path

$portable = Join-Path $root 'portable'
if (Test-Path -LiteralPath $portable -PathType Container) {
    $destDir = Join-Path $portable 'whisper-models'
    $flavour = '便携版（portable 文件夹）'
} else {
    $destDir = Join-Path $env:LOCALAPPDATA 'app.concat.editor\whisper-models'
    $flavour = '安装版（LOCALAPPDATA）'
}
$dest = Join-Path $destDir $name

function Test-Model([string]$path) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
    if ((Get-Item -LiteralPath $path).Length -ne $bytes) { return $false }
    return ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -eq $sha)
}

function Done([string]$text) {
    Write-Host ''
    Write-Host $text -ForegroundColor Green
    Write-Host ''
    Read-Host '按回车键关闭'
    exit 0
}

function Fail([string]$text) {
    Write-Host ''
    Write-Host $text -ForegroundColor Red
    Write-Host ''
    Read-Host '按回车键关闭'
    exit 1
}

Write-Host '==============================================' -ForegroundColor Cyan
Write-Host ' 星籁剪辑 · 中文语音模型安装（Small Multilingual）' -ForegroundColor Cyan
Write-Host '==============================================' -ForegroundColor Cyan
Write-Host ''
Write-Host "目标位置：$destDir（$flavour）"
Write-Host ''

if (Test-Model $dest) {
    Done "已安装，无需重复操作：`n$dest"
}

New-Item -ItemType Directory -Force -Path $destDir | Out-Null

# 1) 随包的模型文件（把 ggml-small.bin 放进 安装目录\模型\ 即可离线安装）
$local = Join-Path $root '模型\ggml-small.bin'
if (-not (Test-Path -LiteralPath $local -PathType Leaf)) {
    $local = Join-Path $root '模型\ggml-small.bin'
}
if (Test-Path -LiteralPath $local -PathType Leaf) {
    Write-Host "找到随包模型：$local"
    Write-Host '正在复制（约 488 MB）…'
    Copy-Item -LiteralPath $local -Destination $dest -Force
    if (Test-Model $dest) { Done "安装成功（离线）：`n$dest" }
    Remove-Item -LiteralPath $dest -Force -ErrorAction SilentlyContinue
    Write-Host '随包文件校验失败，改为联网下载。' -ForegroundColor Yellow
}

# 2) 网络：curl.exe 默认不读 Windows 的系统代理，这里从注册表取出来传给它。
$proxyArgs = @()
try {
    $reg = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings' -ErrorAction Stop
    if ($reg.ProxyEnable -eq 1 -and $reg.ProxyServer) {
        $server = $reg.ProxyServer
        if ($server -match '=') {
            $server = ($server -split ';' | Where-Object { $_ -match '=' } | Select-Object -First 1)
            if ($server) { $server = ($server -split '=', 2)[1] }
        }
        if ($server -match '^(?:https?://)?([^/:]+):(\d+)$') {
            $proxyArgs = @('--proxy', "http://$($Matches[1]):$($Matches[2])")
            Write-Host "使用系统代理：http://$($Matches[1]):$($Matches[2])"
        }
    }
} catch { }

$urls = @(
    'https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.bin',
    'https://hf-mirror.com/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.bin',
    'https://github.com/jub0t/Concat/releases/download/models-v1/ggml-small.bin'
)
$part = "$dest.part"
$curl = Get-Command curl.exe -ErrorAction SilentlyContinue

foreach ($url in $urls) {
    Write-Host ''
    Write-Host "下载：$url"
    try {
        if ($curl) {
            & $curl.Source -L --fail --retry 2 --connect-timeout 20 -C - @proxyArgs -o $part $url
        } else {
            Write-Host '（没有 curl.exe，使用 PowerShell 下载）'
            Invoke-WebRequest -Uri $url -OutFile $part -UseBasicParsing
        }
    } catch {
        Write-Host "本次失败：$($_.Exception.Message)" -ForegroundColor Yellow
    }
    if (Test-Path -LiteralPath $part -PathType Leaf) {
        $size = (Get-Item -LiteralPath $part).Length
        if ($size -eq $bytes -and (Test-Model $part)) {
            Move-Item -LiteralPath $part -Destination $dest -Force
            Done "安装成功：`n$dest`n`n请重启「星籁剪辑」，在 设置 → 转录器 中应看到「已安装 1 个」。"
        }
        Write-Host "下载不完整或校验失败（$size B），换下一个源…" -ForegroundColor Yellow
        Remove-Item -LiteralPath $part -Force -ErrorAction SilentlyContinue
    }
}

Fail @"
自动下载失败（三个源都没成功）。

请手动下载（任一地址）：
  https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.bin
  https://github.com/jub0t/Concat/releases/download/models-v1/ggml-small.bin

然后把文件重命名为 ggml-small.bin，放到：
  $destDir

如果浏览器能打开下载页而脚本不能，通常是代理设置问题；
在「设置 → 网络和 Internet → 代理」里确认系统代理可用，或先手动下载。
"@
