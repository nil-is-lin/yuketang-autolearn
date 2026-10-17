# 一键发版：构建 → 打包 → 打 tag → 推送 → 创建 GitHub Release
#
# 为什么不用 CI：GitHub Actions 的工作流文件需要 gh token 带 workflow 权限，
# 而设备码授权在部分网络下连不上 github.com/login/device/code。
# 发版本身只需要 repo 权限，所以这里用本机构建 + gh release create，效果一样。
#
# 用法：
#   pwsh -File scripts\release.ps1              # 按 Cargo.toml 里的版本发版
#   pwsh -File scripts\release.ps1 -DryRun      # 只构建和打包，不推送/不建 Release
#   pwsh -File scripts\release.ps1 -Notes 说明.md

param(
    [switch]$DryRun,
    [string]$Notes = "CHANGELOG.md",
    [string]$Target = "windows-x64"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# 1) 版本号：以 Cargo.toml 为准，避免 tag 和二进制对不上
$ver = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
if (-not $ver) { throw "读不到 Cargo.toml 里的 version" }
$tag = "v$ver"
$asset = "yuketang-autolearn-$tag-$Target.exe"
Write-Host "版本: $ver  标签: $tag  产物: $asset"

# 2) 构建（资源编译需要 rc.exe，找不到只是没图标，不影响）
if (-not $env:PATH.Contains("Windows Kits")) {
    $kit = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin\*\x64\rc.exe" -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($kit) { $env:PATH = "$($kit.DirectoryName);$env:PATH" }
}
cargo test --release
if ($LASTEXITCODE -ne 0) { throw "测试没通过，停止发版" }
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "构建失败" }

# 3) 打包 + 校验和
New-Item -ItemType Directory -Force -Path dist | Out-Null
Copy-Item "target\release\yuketang-autolearn.exe" "dist\$asset" -Force
$hash = (Get-FileHash "dist\$asset" -Algorithm SHA256).Hash.ToLower()
"$hash  $asset" | Out-File -Encoding ascii "dist\SHA256SUMS.txt"
Write-Host "SHA256: $hash"

if ($DryRun) {
    Write-Host "`n-DryRun：已构建并打包到 dist\，未推送、未创建 Release。" -ForegroundColor Yellow
    Get-ChildItem dist | Select-Object Name, Length
    exit 0
}

# 4) 提交 + 打 tag + 推送（有改动才提交）
git add -A
if (git status --porcelain) {
    git -c user.name="yuketang-autolearn" -c user.email="dev@example.com" commit -q -m "release: $tag"
}
if (-not (git tag -l $tag)) {
    git -c user.name="yuketang-autolearn" -c user.email="dev@example.com" tag -a $tag -m "$tag"
}
git push origin HEAD
git push origin $tag

# 5) 创建 Release（repo 权限即可）
if (gh release view $tag 2>$null) {
    gh release upload $tag "dist\$asset" "dist\SHA256SUMS.txt" --clobber
    Write-Host "已更新已有 Release：$tag"
} else {
    gh release create $tag "dist\$asset" "dist\SHA256SUMS.txt" --title "$tag" --notes-file $Notes
}
Write-Host "完成：$(gh release view $tag --json url --jq .url)"
