# avm 设计方案

面向游戏自动化的精简 Android x86_64 运行时与设备底座。自动化脚本由外部工具（Airtest / Appium / uiautomator2）实现；本项目提供运行时（VMM）、实例池、辅助能力（adb 通道）与 NSH 管理面板。

## 1. 定位与边界

- **做**：VM 生命周期、Hypervisor 抽象、virtio 设备模型、实例池/快照、HTTP API、NSH 面板、adb 辅助通道。
- **不做**：自动化脚本引擎（交给外部工具）、guest 侧 Venus/gfxstream 驱动（用预构建 AOSP 镜像）、GPU 驱动自研。

## 2. 技术选型

| 层 | 选型 | 依据 |
|----|------|------|
| 内存与引导 | `vm-memory`、`linux-loader`、`acpi_tables` | rust-vmm 稳定组件，支持 Windows |
| virtio 设备 | `virtio-queue`、`virtio-bindings` | 规范常量与队列管理 |
| Hypervisor | 自研 WHPX FFI（`WinHvPlatform.dll`） | rust-vmm 无稳定 WHPX crate；参考 libkrun `src/whp/` |
| 后续跨平台 | KVM / HVF，同一 trait | 分层隔离 |
| 3D 图形（M9） | `rutabaga_gfx` + `vhost-device-gpu` + `ash` | 避免从零实现 Venus 协议 |
| 管理面板 | NSH 纯 Rust | 已确认 `Tray` / `set_tray` Rust API |
| daemon/API | tokio + axum | M6 起引入 |

## 3. 进程模型

`avmd` 后台守护进程持有全部 VM 实例与 HTTP API；NSH 面板与 CLI 均为客户端。面板关闭按钮隐藏到托盘，实例继续运行。

```
┌─────────┐  ┌─────────┐      HTTP       ┌──────────────────────┐
│ avm CLI │  │ NSH面板 │ ──────────────► │  avmd (daemon)       │
└─────────┘  └─────────┘                 │  ┌─ InstancePool     │
                                          │  ├─ VmInstance ×N   │
外部自动化工具（Airtest/Appium/u2）        │  │   ├ hypervisor   │
      │ adb (M3+)                         │  │   ├ devices      │
      └──────────────────────────────────►│  │   └ vcpu loop    │
                                          │  └─ HTTP API (axum) │
                                          └──────────────────────┘
```

## 4. 目录结构

```
avm/
├── Cargo.toml          workspace 根 + avm CLI
├── crates/
│   ├── core/           错误 / 配置 / 类型（InstanceState、InstanceConfig）
│   ├── hypervisor/     trait + WHPX 实现（+KVM/HVF，M8）
│   ├── vmm/            builder / boot / bus / vcpu
│   ├── devices/        virtio-blk / net / gpu / input / UART
│   ├── daemon/         avmd 后台服务（实例注册表）
│   ├── pool/           实例池 / 快照
│   ├── assist/         adb 通道 / 辅助能力
│   ├── api/            HTTP 路由与 DTO
│   ├── client/         CLI 与面板共用的客户端
│   └── gfx/            M9：rutabaga_gfx 接入层（后置，暂未建）
├── panel/              NSH 纯 Rust 面板
└── src/main.rs         CLI 入口（doctor/list/start/stop）
```

## 5. 里程碑

| 阶段 | 内容 | 出口标准 |
|------|------|---------|
| M0 | WHPX FFI + 最小分区 + toy guest | 串口打印 |
| M1 | Linux 引导（linux-loader） | guest 进入 shell |
| M2 | PCI + virtio-blk（acpi-tables） | 挂载 rootfs |
| M3 | virtio-net | adb 连通 |
| M4a | virtio-gpu 2D scanout | 可截图 |
| M5 | Android x86_64 完整启动（goldfish kernel） | 桌面可见、adb 可用 |
| M6 | daemon + api + client | CLI 可连接守护进程 |
| M7 | NSH 面板、托盘最小化 | 面板可管理实例 |
| M8 | KVM / HVF 跨平台 | 多平台运行 |
| **M9** | **rutabaga_gfx + Venus 3D 加速** | **3D 游戏可运行** |

M9 后置理由：Windows host 适配存在不确定性（`RutabagaDescriptor` 在 Windows 为 handle、`rutabaga` 为 `!Send`、部分通道仅考虑 Linux），不阻塞前八阶段的验证。

## 6. 3D 加速实施要点（M9）

> ⚠️ M9 路线已修正：Venus 官方不支持 Windows 宿主（Linux fd / Android dma_buf 限定），
> 主路径改为 **gfxstream**（gfxstream_vk 在 Windows 已工作多年）。
> 详细工作流、风险与决策点见 docs/PLAN-M9.md。

- Guest 侧 Venus / gfxstream 驱动由预构建 AOSP 镜像提供，不自研。
- `rutabaga_gfx` 处理 3D hypercall、Venus capset、资源与 fence；`vhost-device-gpu` 提供 virtio 1.2 设备模型。
- 自研范围：设备接入自有总线、`ash` 初始化 host Vulkan、帧缓冲呈现到窗口、Windows handle 与线程适配。
- 若 Venus 在 Windows 受阻，回退路径为 2D 显示（M4a 已可用），不影响自动化基本功能。

## 7. 核心接口契约（骨架已定义）

- `Hypervisor`（hypervisor/src/lib.rs）：`backend() / create_partition()`
- `Partition`：`map_memory / run_vcpu -> VcpuExit / set_regs`
- `VcpuExit`：`Halt / Io / Mmio / Unknown`
- `BusDevice`（devices）：`id / read / write`，M2 扩展 PCI BAR
- `InstanceRegistry`（daemon）：注册/查询/列表
- `InstancePool`（pool）：`add / acquire / release`，快照恢复在 M8 后接入
- `AvmError / AvmResult`（core）：全 workspace 统一错误

## 8. 主要风险

| 风险 | 应对 |
|------|------|
| WHPX 环境未开启或与 HAXM 冲突 | `doctor` 前置检测 |
| Android 镜像缺 virtio 驱动 | 选用 goldfish kernel，实测验证 |
| NSH Tray 未进入 crates.io 0.4.0 | pin 已验证 commit |
| M9 Windows GPU 适配不确定 | 后置；2D 路径保底 |
| 3D 重度游戏帧率 | 依赖宿主 Vulkan 驱动，无保证 |
