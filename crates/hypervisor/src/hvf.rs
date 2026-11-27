//! HVF 存根（M8）：macOS Hypervisor.framework。
//!
//! M8 出口为 KVM（Linux 可验证）+ HVF 接口就位；HVF FFI 实装
//! （hv_vm_create/hv_vcpu_run 等）需要 macOS 宿主，届时在 trait 不变的前提下补齐。

use avm_core::{AvmError, AvmResult};

use crate::{Backend, Hypervisor, Partition};

/// HVF 后端（当前为占位）。
pub struct HvfHypervisor;

impl HvfHypervisor {
    pub fn new() -> AvmResult<Self> {
        Err(AvmError::Hypervisor(
            "HVF FFI 待 macOS 宿主实装（M8 接口已就位）".into(),
        ))
    }
}

impl Hypervisor for HvfHypervisor {
    fn backend(&self) -> Backend {
        Backend::HVF
    }

    fn create_partition(&self, _vcpus: u32, _memory_mib: u64) -> AvmResult<Box<dyn Partition>> {
        Err(AvmError::Hypervisor(
            "HVF FFI 待 macOS 宿主实装（M8 接口已就位）".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hvf_stub_reports_hvf_backend_and_errors() {
        assert_eq!(HvfHypervisor.backend(), Backend::HVF);
        let h = HvfHypervisor;
        assert!(h.create_partition(1, 64).is_err());
        assert!(HvfHypervisor::new().is_err());
    }
}
