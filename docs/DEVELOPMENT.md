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

### M3：virtio-net ✅ 代码完成（真机 TAP/adb 连通待 Windows WHPX）
- `devices/virtio_net.rs`：rx/tx 双队列 virtio-net（VIRTIO_NET_HDR 12B、MAC+链路状态 config）；`NetBackend` trait + `LoopbackBackend` 回环后端。
- `vmm/virtio_pci.rs`：`VirtioNetPci` 适配器（0x1AF4:0x1000，网络类 0x02，BAR0 0x100 MAC/链路 config、0x108 notify）。
- 61 单测全绿 0 警告（含 tx→后端、后端→rx 注入、自环 roundtrip、缓冲不足报错、无帧 0 used）。
- 待办：真机 TAP 后端（Windows TAP/WSL 网桥）+ guest 驱动探测 → adb over TCP。
### M4a：virtio-gpu 2D scanout ✅ 代码完成（真机显示待 Windows WHPX）
- `devices/virtio_gpu.rs`：controlq 最小 2D 协议（GET_DISPLAY_INFO / RESOURCE_CREATE_2D / UNREF / ATTACH_BACKING / TRANSFER_TO_HOST_2D / SET_SCANOUT / RESOURCE_FLUSH），B8G8R8A8，固定 800x600 单显示头；`framebuffer()` 即截图出口。
- `vmm/virtio_pci.rs`：`VirtioGpuPci`（0x1AF4:0x1010，显示类 0x03）。
- 68 单测全绿 0 警告（全链路 create→attach→transfer→scanout→flush→逐像素截图比对）。
- 坑：测试多命令复用 avail ring 时**不可重置 avail idx 为 0**——设备侧 next_avail 已前进，重置后 (idx−next_avail) 回绕导致链不被消费。
- 待办：真机 scanout 呈现到窗口（M4b）。
### M5：Android x86_64 完整启动 ✅ 代码完成（真机启动待 Windows WHPX + Android 镜像）
- `boot.rs::android`：Android 引导 cmdline 预设（console=ttyS0、androidboot.hardware=android_x86_64、selinux=permissive，可追加自定义参数）。
- `acpi.rs`：MADT 生成（N×LAPIC + IOAPIC，x86 LAPIC MMIO 0xFEE00000），XSDT 登记 MADT+SSDT；build(vcpus) 校验 1..=255。
- `deviceset.rs`：DeviceSet 编排 blk/net/gpu 三设备挂 PCI 总线；`as_any` 下转型直取设备（截图/容量校验出口）；`android_default()`（2GiB 盘、固定 MAC 52:54:00:41:56:4D）。
- 79 单测全绿 0 警告。
- 待办：真机取 Android-x86 镜像（goldfish kernel + rootfs）→ WHPX 全链启动 → adb over TCP 连通。
### M6：daemon + api + client ✅ 代码完成
- `api`：axum HTTP API（/health、GET/POST /vms、GET/DELETE /vms/{id}、POST /vms/{id}/boot|stop）；VmStore 内存状态机（Created→Booting→Running，非法迁移 409）；tower oneshot 路由测试。
- `daemon`：`run_daemon(listen)` 阻塞启动（内建 tokio multi-thread runtime）+ `serve()` 嵌入式；轻量 InstanceRegistry（pool 骨架依赖保留）。
- `client`：ureq 阻塞客户端（health/list/create/get/boot/stop/delete，404→None/false 映射）。
- CLI：`avm daemon --listen` / `list` / `create` / `boot` / `stop` / `delete`，`--url` 指定 daemon。
- 验证：84 单测全绿 0 警告；E2E 实跑（daemon 7879 + CLI create→boot→list→stop→delete 全链路）。
- 坑：①`#[tokio::test]` 默认 current_thread runtime，测试内 ureq 阻塞调用会饿死同 runtime 的 serve 任务→用 flavor="multi_thread"；②pkill -f "cargo test" 会连自己的 bash 包装一起杀；③axum 0.8 路由捕获用 {id}（:id 会 panic）；④grep -c 无匹配 exit 1 会断 && 链。
### M7：NSH 面板 + 托盘 ✅ 代码完成（原生托盘壳待 Windows）
- `panel`：视图模型与 GUI 解耦——`Panel`（daemon 客户端 + 视图缓存 + create/boot/stop/delete 动作）、`PanelView::render` 对齐文本表、`TrayMenu`/`TrayItem`/`TrayAction` 托盘菜单模型（set_tray 数据源）、`on_tray_action` 事件数据路径。
- `setup_tray` 为 NSH 存根；Windows 真机阶段接 feature `nsh`（NSH Tray API pin commit）。
- 验证：86 单测全绿 0 警告；面板 E2E（panel→daemon 起服务：create→boot→stop→delete 全链）。
- 面板关闭 = 隐藏到托盘：实例由 avmd 持有，面板只是客户端视图（架构约束保持）。
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
