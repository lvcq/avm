//! WHPX FFI 骨架（M0 实现）。
//!
//! 依赖 `WinHvPlatform.dll` 的 `WinHvCreatePartition` / `WinHvMapGpaRange`
//! / `WinHvRunVirtualProcessor` 等 API。参考 libkrun `src/whp/`。
//! 骨架阶段仅定义 API 面，不链接 Windows 库（本 crate 在非 Windows 上编译为空壳）。

use avm_core::{AvmError, AvmResult};
use crate::{Backend, Hypervisor, Partition, VcpuExit};

/// WHPX 后端。Windows 上加载 WinHvPlatform.dll，其它平台构造即报错。
#[derive(Debug, Default)]
pub struct WhpHypervisor;

impl WhpHypervisor {
    pub fn new() -> AvmResult<Self> {
        if cfg!(windows) {
            // M0: LoadLibrary("WinHvPlatform.dll") + doctor 前置检测在此展开
            Ok(Self)
        } else {
            Err(AvmError::Hypervisor(
                "WHPX 仅在 Windows 宿主可用（M0 里程碑实现）".into(),
            ))
        }
    }
}

impl Hypervisor for WhpHypervisor {
    fn backend(&self) -> Backend {
        Backend::WHPX
    }

    fn create_partition(&self, _vcpus: u32, _memory_mib: u64) -> AvmResult<Box<dyn Partition>> {
        Err(AvmError::Hypervisor("WHPX 分区创建：M0 实现".into()))
    }
}

/// WHPX Partition 句柄（M0 落地时持有 HPARTITION 与 vcpu handle）。
pub struct WhpPartition;

impl Partition for WhpPartition {
    fn map_memory(&mut self, _gpa: u64, _host: &mut [u8]) -> AvmResult<()> {
        Err(AvmError::Hypervisor("M0".into()))
    }

    fn run_vcpu(&mut self, _vcpu: u32) -> AvmResult<VcpuExit> {
        Err(AvmError::Hypervisor("M0".into()))
    }

    fn set_regs(&mut self, _vcpu: u32, _rip: u64, _rsp: u64) -> AvmResult<()> {
        Err(AvmError::Hypervisor("M0".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_windows_host_is_rejected() {
        if !cfg!(windows) {
            assert!(WhpHypervisor::new().is_err());
        }
    }
}
