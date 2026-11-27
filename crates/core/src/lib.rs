//! avm-core：错误 / 配置 / 公共类型。
//!
//! 全 workspace 的错误统一通过 `AvmError` / `AvmResult` 表达。
//! 规则（与 miao-lab 同源）：workspace 内禁用 unwrap/expect/panic，
//! 一切可失败路径必须返回 Result。

use std::fmt;

#[derive(Debug)]
pub enum AvmError {
    /// Hypervisor 层错误（WHPX/KVM/HVF 调用失败）
    Hypervisor(String),
    /// 设备模型错误（virtio 队列、寄存器访问越界等）
    Device(String),
    /// 引导失败（内核格式、内存布局冲突）
    Boot(String),
    /// 配置错误
    Config(String),
    /// daemon / api / client 通信错误
    Ipc(String),
}

impl fmt::Display for AvmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AvmError::Hypervisor(m) => write!(f, "hypervisor: {m}"),
            AvmError::Device(m) => write!(f, "device: {m}"),
            AvmError::Boot(m) => write!(f, "boot: {m}"),
            AvmError::Config(m) => write!(f, "config: {m}"),
            AvmError::Ipc(m) => write!(f, "ipc: {m}"),
        }
    }
}

impl std::error::Error for AvmError {}

pub type AvmResult<T> = Result<T, AvmError>;

pub mod log;

/// VM 实例状态机（pool 管理与面板展示共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstanceState {
    Created,
    Booting,
    Running,
    Paused,
    Snapshot,
    Stopped,
    Failed,
}

/// 一台 VM 的静态配置。
#[derive(Debug, Clone)]
pub struct InstanceConfig {
    pub id: String,
    pub vcpus: u32,
    /// MiB
    pub memory_mib: u64,
    /// Android 镜像 / rootfs 路径
    pub image_path: String,
    pub enable_net: bool,
    pub enable_gpu: bool,
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            vcpus: 2,
            memory_mib: 2048,
            image_path: String::new(),
            enable_net: true,
            enable_gpu: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_variant_tag() {
        let e = AvmError::Boot("bad kernel".into());
        assert_eq!(e.to_string(), "boot: bad kernel");
    }

    #[test]
    fn default_config_is_sane() {
        let c = InstanceConfig::default();
        assert_eq!(c.vcpus, 2);
        assert!(c.enable_net);
        assert!(!c.enable_gpu);
    }
}
