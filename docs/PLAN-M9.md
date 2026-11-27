# M9 规划：3D 加速（rutabaga_gfx + gfxstream）

> 状态：规划（2026-11）。前置：M0–M8 代码完成 + `docs/VERIFICATION-WHPX.md` 真机验证。
> 本文修正原 DESIGN.md 的 M9 技术路线：**主路径从 Venus 改为 gfxstream**。

## 1. 调研结论（2026-11 实证）

| 事实 | 来源 | 对 M9 的影响 |
|------|------|------------|
| Venus 宿主要求：Linux（VK_KHR_external_memory_fd）或 Android（dma_buf EXT） | mesa 官方文档 docs.mesa3d.org/drivers/venus | **Windows 宿主走不了 Venus**——原设计主路径作废 |
| gfxstream_vk 在 Windows 上已工作多年；mesa 有 `gfxstream/guest/platform/windows` | rutabaga_gfx issue #24 讨论 | **gfxstream 是 Windows 宿主唯一现实路径** |
| rutabaga_gfx 已开源独立演进（magma-gpu，v0.1.76，2025-12）；`RutabagaConnection` 文档明确「Only Linux hosts are considered at the moment」 | crosvm.dev / magma-gpu | 需要 Windows 适配层（handle/fd 转换），且要 pin 版本+补丁 |
| `RutabagaDescriptor` 在 Windows 是 HANDLE、Unix 是 fd；guest-memory 导入句柄仅 Unix 存在 | crosvm FFI 源码 | 自研代码必须做 `HandleKind` 抽象；blob 资源路径 Windows 要绕行 |
| Kumquat（crosvm 的窗口合成器）+ gfxstream backend 在 Windows 可编译 | issue #24 | 呈现层可参考 Kumquat 的 Windows 窗口/swapchain 做法 |
| vhost-device-gpu（rust-vmm）定位独立进程做设备模型；cross-domain 重构进行中 | issue #24 / Kernel Recipes 2025 | M9 不引入 vhost-user 进程拆分——单进程内直接接 rutabaga，降低复杂度 |
| 完整窗口呈现只支持 dma_buf/dma_fence 平台 | magma-gpu README | Windows 用 WSI swapchain 呈现，不走 dma_buf 路径 |

## 2. 目标与非目标

**目标**：Android x86_64 guest 内的 GLES/Vulkan 应用经 gfxstream 直通宿主 GPU 渲染；窗口呈现 + 帧率可量化。

**非目标**：
- ❌ Venus on Windows（平台不支持，永久排除）
- ❌ vhost-user 进程拆分（vhost-device-gpu 暂缓，单进程直连）
- ❌ cross-domain / Wayland passthrough（Windows 无意义）
- ❌ 窗口合成器自研（最小 scanout 即可）

## 3. 分层架构

```
Guest (Android x86_64)
  gfxstream guest 驱动（预构建 AOSP 镜像，不自研）
        │ virtio-gpu capset = gfxstream-vulkan / x-gfxstream-gles
┌───────▼─────────────────────────────────────┐
│ avm crates/gfx                              │
│  ├─ gpu_3d.rs    virtio-gpu 设备 3D 扩展：   │  ← M4a 的 2D 设备升级（capset 协商、
│  │               CREATE_3D/BLOB、CTX 系命令  │    context types、blob 资源）
│  ├─ rutabaga.rs RutabagaBuilder 接入：       │
│  │               capset 查询、资源/CTX 分派、 │
│  │               fence 事件循环              │
│  ├─ handle_win.rs / handle_unix.rs          │  ← RutabagaHandle 抽象（fd/HANDLE）
│  └─ present_win.rs WSI 呈现（ash+win32）     │  ← M4b FrameSink 的 3D 版
└───────┬─────────────────────────────────────┘
   Host Windows
     gfxstream backend（C++，动态链接）→ ANGLE/D3D 或 Vulkan ICD
```

自研边界（同原设计）：设备接入自有总线、`ash` 初始化 host Vulkan、帧呈现到窗口、Windows handle/线程适配；**gfxstream guest 驱动与 rutabaga/gfxstream 本体不自研**。

## 4. 工作流（每段独立出口）

| 段 | 内容 | 出口判据 | 依赖 |
|----|------|---------|------|
| **W9.0 预研** | 拉 rutabaga_gfx v0.1.7x + gfxstream 源码；Windows 编译实验（动态链接 gfxstream backend、ANGLE vs Vulkan ICD 二选一）；M4a 设备升级面评估 | Windows 上 demo 程序经 gfxstream 画出一个三角形到窗口 | 真机 Windows 宿主 |
| **W9.1 设备 3D 扩展** | virtio_gpu.rs 升级：context types（GFXSTREAM=10 等 capset）、RESOURCE_CREATE_3D/BLOB、CTX_ATTACH/DETACH、EXEC_COMMAND 命令流转发 | 单测：capset 协商 + 命令流分派到 Rutabaga mock | Linux |
| **W9.2 rutabaga 接入** | `crates/gfx`：RutabagaBuilder（GfxstreamBackend）、capset 查询、resource/ctx API 映射、fence 处理；`RutabagaHandle` fd/HANDLE 双实现 | Linux：gfxstream software backend（swiftshader 路径）跑通 capset 查询 + 资源创建 | W9.0 选型 |
| **W9.3 blob 资源 + 主存映射** | blob 资源创建/映射（Windows 无 guest-memory 句柄导入路径——用共享内存段 + host 侧映射绕行）；TRANSFER_TO_HOST_3D 读回 | 资源像素经 CPU 路径读回正确（无 GPU 呈现） | W9.2 |
| **W9.4 窗口呈现** | ash + win32 swapchain；fence 驱动的 dirty-flush → present；接入 daemon `screenshot` API | 真机：guest glmark2-es2 / vkmark 出画面 | W9.3 + WHPX 真机 |
| **W9.5 Android guest 联调** | 预构建 AOSP 镜像（含 gfxstream guest 驱动）；capset 协商实测；SurfaceFlinger→scanout | adb 可用 + Android 桌面 GPU 加速渲染 | W9.4 + M5 真机 |
| **W9.6 性能基线** | glmark2/vkmark 分数、帧率、CPU 占用；与 2D 软渲染对比 | 数据进 docs/；决定是否投入 vhost-device-gpu 拆分 | W9.5 |

## 5. 风险与回退

| 风险 | 等级 | 应对 |
|------|------|------|
| rutabaga Windows 适配缺失（RutabagaConnection 仅 Linux） | 高 | W9.0 先做编译实验定生死；不行则自写 thin shim（只桥接我们用到的 API 面） |
| gfxstream Windows 后端构建复杂（C++ 依赖链） | 高 | 用预构建 gfxstream 动态库；ANGLE（D3D11/12）优先于原生 Vulkan ICD |
| `rutabaga` 线程模型与 avmd 单实例模型冲突（`!Send`） | 中 | gfx 运行在独立线程 + channel 命令队列（模型化 M4b FrameSink 的做法） |
| Windows blob 路径无 guest-memory 导入 | 中 | 共享内存段绕行（W9.3）；性能损失先量化再优化 |
| 3D 帧率不满足重度游戏 | 中 | 2D 路径（M4a/M4b）已保底自动化；3D 是增强不是门槛 |
| gfxstream 版本漂移 | 低 | pin commit + 变更日志盯订阅 |

## 7. 宿主无 Vulkan 的处理（W9.0 关键分支）

宿主 GPU 驱动未装 Vulkan ICD（或 ICD 与 WHPX 虚拟化冲突）时的三级降级：

| 级别 | 方案 | 代价 | 判据 |
|------|------|------|------|
| L0 正常 | 宿主 Vulkan ICD 直通 | 无 | `vkCreateInstance` 成功且 device 数 ≥ 1 |
| L1 **ANGLE→D3D** | gfxstream 后端走 ANGLE（ translating GLES→D3D11/12）；gfxstream 自带 ANGLE 支持，无需宿主 Vulkan | 3D 性能 ~70-90% of L0；Vulkan-only 游戏不可用 | 游戏为 GLES 主体时首选，Windows 遍地 D3D 驱动，成功率最高 |
| L2 **软件 Vulkan** | 宿主装 lavapipe（Mesa 软件渲染器）或 SwiftShader，gfxstream_vk 照常跑 | CPU 光栅化，帧率低；但功能完整，联调/CI 不受 GPU 驱动拖累 | 预研与无 GPU 云机联调用 |
| L3 2D 保底 | 退回 M4a/M4b 2D scanout（当前已实现） | 无 3D 加速 | 自动化基本功能不受影响，永久兜底 |

**doctor 集成**：`avm doctor` 增加 GPU 探测——`ash` 尝试建 instance 枚举 device（L0），失败则检测 `VK_DRIVER_FILES`/lavapipe（L2），两者皆无提示安装流程并标记 L3。`avm verify` 输出当前可用级别，选后端时显式 `--gfx {auto,angle,sw,2d}`。

**安装路径（Windows 无 Vulkan 时）**：
1. 先确认显卡驱动官网版是否带 Vulkan（NVIDIA/AMD 默认带；Intel 核显较旧则无）
2. 不想动驱动 → 直接下 SwiftShader Windows 构建放 `VK_ICD_FILENAMES` 指向（L2，零侵入）
3. 长期运行 → ANGLE 路线（L1），gfxstream 预构建库自带

## 8. 里程碑时序与依赖

```
真机 WHPX 验证（当前关键路径）
   └─ M5 Android 真机启动（Stage 5）
        └─ W9.0 预研（Windows 编译实验）→ W9.1（可并行，纯 Linux 单测）
             └─ W9.2 → W9.3 → W9.4 → W9.5 → W9.6
```

**决策点**（W9.0 末）：
1. ANGLE vs 原生 Vulkan ICD
2. rutabaga shim 深度（直接用 vs 自写桥接层）
3. 若 W9.0 双双受阻 → M9 降级为「2D 路径 + CPU 光栅化增强」，3D 永久挂起（不影响自动化主线）
