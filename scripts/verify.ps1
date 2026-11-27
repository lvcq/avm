# avm Windows WHPX 真机验证脚本
# 用法（管理员 PowerShell）：
#   .\scripts\verify.ps1              # 全部阶段
#   .\scripts\verify.ps1 -MaxStage 2  # 只跑到指定阶段
param(
    [int]$MaxStage = 5
)

$ErrorActionPreference = "Stop"
$expectedToy = "avm toy guest OK"

function Fail($stage, $msg) {
    Write-Host "== Stage $stage FAIL ==" -ForegroundColor Red
    Write-Host $msg
    exit 1
}

Write-Host "== Stage 1: 环境与 doctor ==" -ForegroundColor Cyan
cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { Fail 1 "cargo build 失败" }
$doctorOut = .\target\release\avm.exe doctor 2>&1
$doctorOut | ForEach-Object { Write-Host $_ }
if ($LASTEXITCODE -ne 0) {
    Fail 1 "doctor 未通过：确认已在「启用或关闭 Windows 功能」开启 Hyper-V 与虚拟机平台，且无 HAXM 冲突"
}
Write-Host "Stage 1 PASS" -ForegroundColor Green

Write-Host "== Stage 2: toy guest 串口 ==" -ForegroundColor Cyan
$toyOut = .\target\release\avm.exe verify 2>&1
$toyOut | ForEach-Object { Write-Host $_ }
if ($LASTEXITCODE -ne 0) { Fail 2 "toy guest 串口验证失败" }
if (-not ($toyOut -match [regex]::Escape($expectedToy))) { Fail 2 "未看到 '$expectedToy'" }
Write-Host "Stage 2 PASS" -ForegroundColor Green

Write-Host "== Stage 3: bzImage 加载（无真机执行，验证加载器） ==" -ForegroundColor Cyan
# 需要预先放置 assets：kernel bzImage（如 android-x86 的内核）
if (Test-Path "assets\bzImage") {
    Write-Host "发现 assets\bzImage，加载验证："
    .\target\release\avm.exe start --kernel assets\bzImage --dry-run
    if ($LASTEXITCODE -ne 0) { Fail 3 "bzImage 加载失败" }
    Write-Host "Stage 3 PASS" -ForegroundColor Green
} else {
    Write-Host "Stage 3 SKIP（无 assets\bzImage；从 android-x86 ISO 提取后重跑）" -ForegroundColor Yellow
}

Write-Host "== Stage 4: daemon + 面板链路 ==" -ForegroundColor Cyan
$daemonJob = Start-Job { .\target\release\avm.exe daemon --listen 127.0.0.1:7878 }
Start-Sleep -Seconds 2
.\target\release\avm.exe create vm-verify --vcpus 2 --mem 2048
.\target\release\avm.exe boot vm-verify
.\target\release\avm.exe list
.\target\release\avm.exe delete vm-verify
Stop-Job $daemonJob; Remove-Job $daemonJob
if ($LASTEXITCODE -ne 0) { Fail 4 "daemon 链路失败" }
Write-Host "Stage 4 PASS" -ForegroundColor Green

if ($MaxStage -ge 5) {
    Write-Host "== Stage 5: Android 全启动（人工阶段） ==" -ForegroundColor Cyan
    Write-Host @"
按 docs/VERIFICATION-WHPX.md 的 Stage 5 手动步骤：
  1. assets/ 放置 bzImage / initrd.img / ramdisk.img / android.rootfs.img
  2. avm start --kernel assets\bzImage --initrd assets\initrd.img --android
  3. 观察串口到 Android 桌面；adb connect 127.0.0.1:5555
"@
}

Write-Host "ALL PASS (stage <= $MaxStage)" -ForegroundColor Green
