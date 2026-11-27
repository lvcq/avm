# Windows WHPX 真机验证清单

> 目标：把 M0–M8 的代码成果在真实 Windows 宿主上逐段点亮。
> 原则：每段有明确的 PASS 判据，失败即停；上一段绿了才进下一段。

## Stage 1：环境就绪（doctor）

1. 「启用或关闭 Windows 功能」→ 勾选 **Hyper-V** 与 **虚拟机平台**（WHPX 依赖后者）。
2. 卸载/停用 Android 模拟器 HAXM（与 WHPX 互斥）。
3. 管理员 PowerShell：
   ```powershell
   cargo build --release --workspace
   .\target\release\avm.exe doctor
   ```
**PASS 判据**：doctor 全 [✓]，其中含「WHPX 分区 1 vcpu / 64MiB 探针通过」。

## Stage 2：toy guest 串口（M0 真实执行）

```powershell
.\target\release\avm.exe verify
```
**PASS 判据**：串口输出 `avm toy guest OK`，退出码 0。
（该输出由 guest 16 位实模式代码逐字符 OUT 0x3F8 产生——任何一环错都会体现在这里。）

## Stage 3：bzImage 加载（M1 管线）

1. `assets/` 放置内核（如 android-x86 ISO 内 `/kernel`）。
2. `.\target\release\avm.exe start --kernel assets\bzImage --dry-run`
**PASS 判据**：加载器解析 HdrS 成功、kernel/initrd/cmdline 布局打印无错。
（真机 vcpu 执行进入 shell 属 Stage 5。）

## Stage 4：daemon + 面板链路（M6/M7）

```powershell
.\scripts\verify.ps1 -MaxStage 4
```
**PASS 判据**：create → boot → list → delete 全链 0 退出码。

## Stage 5：Android x86_64 完整启动（M5 真机执行，人工）

1. 从 android-x86 ISO 提取：`bzImage`、`initrd.img`、`ramdisk.img`，rootfs 镜像转 virtio-blk 盘。
2. `assets/` 就位后：
   ```powershell
   .\target\release\avm.exe start --kernel assets\bzImage --initrd assets\initrd.img --android
   ```
3. cmdline 自动使用 `boot.rs::android::CMDLINE_TEMPLATE`（console=ttyS0 + androidboot.hardware=android_x86_64）。
4. 观察串口：内核解压 → init → SurfaceFlinger。
5. `adb connect 127.0.0.1:5555`（M3 virtio-net 真机 TAP 后端接通后）。

**PASS 判据**：adb devices 可见设备；`avm screenshot`（经 daemon GPU 帧缓冲）可截 Android 桌面。

## 已知风险

| 风险 | 现象 | 处置 |
|------|------|------|
| WHPX 与 HAXM 冲突 | doctor 分区探针失败 | 卸载 HAXM |
| Hyper-V 未开「虚拟机平台」 | WinHvPlatform.dll 加载失败 | 补勾选并重启 |
| toy guest 无输出 | Stage 2 卡住/退出码 2 | 检查 WHPX RunVirtualProcessor 与 IO exit 的 rax 语义（whp.rs） |
| Android 内核缺 virtio 驱动 | Stage 5 内核 panic 无 rootfs | 换 goldfish 内核或重组 initramfs 加 virtio 模块 |
