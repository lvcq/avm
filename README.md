# avm

面向游戏自动化的精简 Android x86_64 运行时与设备底座（Rust）。

- 设计方案：[docs/DESIGN.md](docs/DESIGN.md)
- 开发指引（AI Agent 参考）：[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)
- 架构图：[docs/ARCHITECTURE.svg](docs/ARCHITECTURE.svg)

## 快速开始

```bash
cargo build --workspace
cargo test --workspace
cargo run -- doctor
```

## 状态

骨架阶段：workspace 全绿（16 单测），trait 接口面已定义，外部 rust-vmm 依赖自 M0 起逐里程碑引入。里程碑 M0–M9 见 DESIGN.md §5。
