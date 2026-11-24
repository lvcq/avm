# avm 开发指引（AI Agent 参考）

本文供后续 AI Agent 在本仓库内开发时遵循。仓库当前为**可编译骨架**（workspace 全绿、16 个单测通过），按里程碑逐步充实。

## 0. 硬性规则

1. **禁 unwrap/expect/panic**：一切可失败路径返回 `AvmResult<T>`（avm-core）。
2. **分层单向依赖**：`core ← hypervisor ← vmm ← daemon ← api ← client ← panel`；devices/assist/pool 只依赖 core（pool 另依赖 daemon）。
3. **vmm 只依赖 `Hypervisor` trait**，不得直接引用任何平台实现模块。
4. **每个里程碑一个出口标准**（见 DESIGN.md §5），达标后才进入下一个。
5. 每完成一个里程碑：`cargo build && cargo test --workspace` 全绿，然后 git 提交（feat(avm): M{n} ...）。

## 1. 当前状态（骨架）

- 全部 crate 已就位，trait 接口面已定义（`Hypervisor/Partition/BusDevice/InstanceRegistry/InstancePool/DaemonClient`）。
- 未实现的方法返回 `AvmError::XXX("M{n} 里程碑实现")`，各 crate 带最小单测。
- 外部依赖（vm-memory 等）**尚未引入**（见根 Cargo.toml 注释块），保持离线可编译；从 M0 开始逐个加入。

## 2. 里程碑实施步骤

### M0：WHPX FFI + 最小分区 + toy guest
1. 仅在 `#[cfg(windows)]` 下实现 `crates/hypervisor/src/whp.rs`：
   - FFI 声明：`WinHvCreatePartition / WinHvSetPartitionProperty / WinHvCreateVP / WinHvMapGpaRange / WinHvRunVirtualProcessor`（链接 `WinHvPlatform.dll`，参考 libkrun `src/whp/`）。
   - `WhpHypervisor::new()` 做前置检测（虚拟机平台功能已启用、WinHvPlatform.dll 可加载），检测逻辑同时被 `avm doctor` 调用。
2. `vmm`：写 toy guest（16 位实模式引导扇区，打印字符到 0x3F8 串口）→ `SerialUart` write 已可用。
3. 出口：`avm start --toy` 在串口打印。

### M1：Linux 引导 ✅ 代码完成（真机验证待 Windows WHPX）
- 已引入 `linux-loader 0.12`（bzimage）+ `vm-memory 0.15`（backend-mmap，与 linux-loader 0.12 对齐）。
- `vmm/boot.rs`：`load_kernel()`（BzImage::load + 自实现 initrd 紧跟内核 2MiB 对齐加载 + Cmdline 写入）、`boot_regs()`（RIP=kernel_load, CS=0x08/base=0）；内存布局见 `layout` 模块。
- 假 bzImage 测试镜像覆盖：合法头加载、非法头拒绝、initrd 越界拒绝、cmdline/initrd 内容校验。
- 待办：真机 bzImage + initrd（Windows WHPX 上 run_vcpu 跳长模式入口）→ guest shell 串口。

### M2：PCI + virtio-blk ✅ 代码完成（真机验证待 Windows WHPX）
- 已引入 `virtio-queue 0.14` + `virtio-bindings 0.2` + `acpi_tables 0.2`（vm-memory 统一 0.16；linux-loader 升 0.13）。
- `vmm/bus.rs`：PCI Type-0 配置空间（1/2/4 字节读，类代码字节序 rev|prog_if|subclass|base）+ BAR0 4KiB 对齐分配与 MMIO 路由。
- `devices/virtio_blk.rs`：virtio-queue Queue + 手写 desc 表测试；请求协议 IN/OUT/UNSUP，数据面 IO 错误回 IOERR 不中止队列；写后读 roundtrip 验证。
- `vmm/virtio_pci.rs`：0x1AF4:0x1001 适配器（BAR0 0x100 容量 config / 0x108 notify 区）。
- `vmm/acpi.rs`：RSDP + XSDT + SSDT（校验和全 0 验证）。
- 待办：真机 bzImage rootfs 挂载（依赖 Windows WHPX vcpu 执行）。

### M3：virtio-net
- 用户态网络（tap/slug 或用户态栈），出口：host 上 `adb connect <guest-ip>:5555` 成功。

### M4a：virtio-gpu 2D scanout
- 实现 scanout → host 窗口呈现；`assist::AdbChannel::screenshot()` 走帧缓冲。
- 出口：可截图。

### M5：Android x86_64 完整启动
- goldfish kernel + Android x86_64 镜像（含 virtio 驱动）；桌面可见、adb 可用。

### M6：daemon + api + client
- 引入 tokio + axum；实现 `api::router()`（路由表已写在 api/src/lib.rs 注释）、`daemon::run_daemon()`；CLI 切换为 `DaemonClient` 模式。
- 出口：`avmd &` 后 `avm list/start/stop` 可用。

### M7：NSH 面板
- `panel`：接入 NSH UI 与 `set_tray`；关闭按钮 → 隐藏到托盘。
- 出口：面板可管理实例。

### M8：KVM / HVF
- `crates/hypervisor/` 增加 `kvm.rs` / `hvf.rs`，实现同一 trait；feature 或运行时探测选择。
- 出口：Linux/macOS 可跑 M1–M5 同套流程。

### M9：3D 加速（后置）
- 见 DESIGN.md §6。`crates/gfx/` 届时新建。

## 3. 编码规范

- 错误：只用 avm-core 的 `AvmError` 变体；新错误类别先在 core 加变体。
- 命名：crate 前缀 `avm-`；模块名小写单词。
- 测试：每个 crate 至少一条 `#[cfg(test)]`；trait 实现必须带可测桩（Null 后端模式延续）。
- FFI：所有 WHPX 调用检查返回 HRESULT，非 0 转 `AvmError::Hypervisor` 并附 API 名。
- 日志：M6 前用 `eprintln`/串口打印；M6 起引入 `tracing`。

## 4. 提交规范

- 格式：`feat(avm): M0 WHPX 最小分区+toy guest 串口打印`（或 fix/refactor/docs）。
- 每个里程碑至少一个提交；提交前 `cargo test --workspace` 必须全绿。
- 文档与代码同 PR/提交更新（DESIGN.md 里程碑表勾选状态可在 §5 表格加 ✅）。

## 5. 验证命令

```bash
cargo build --workspace
cargo test --workspace
cargo run -- doctor          # M0 起逐步填充真实检测
```
