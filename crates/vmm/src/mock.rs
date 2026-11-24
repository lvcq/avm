//! mock hypervisor：测试/无 WHPX 环境下的桩，按脚本回放 vcpu 退出序列。

use avm_core::{AvmError, AvmResult};
use avm_hypervisor::{Backend, Hypervisor, Partition, VcpuExit};
use std::collections::VecDeque;

pub struct MockPartition {
    script: VecDeque<VcpuExit>,
    vcpus: u32,
}

impl Partition for MockPartition {
    fn map_memory(&mut self, _gpa: u64, _host: &mut [u8]) -> AvmResult<()> {
        Ok(())
    }

    fn run_vcpu(&mut self, vcpu: u32) -> AvmResult<VcpuExit> {
        if vcpu >= self.vcpus {
            return Err(AvmError::Hypervisor(format!("vcpu {vcpu} 超出范围")));
        }
        Ok(self.script.pop_front().unwrap_or(VcpuExit::Halt))
    }

    fn set_regs(&mut self, _vcpu: u32, _rip: u64, _rsp: u64) -> AvmResult<()> {
        Ok(())
    }
}

/// 编程式 hypervisor：按脚本回放退出。
pub struct MockHypervisor {
    script: VecDeque<VcpuExit>,
}

impl MockHypervisor {
    /// 预置 vcpu 退出脚本（按序回放，耗尽后返回 Halt）。
    pub fn with_script(script: Vec<VcpuExit>) -> Self {
        Self {
            script: script.into(),
        }
    }
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self::with_script(Vec::new())
    }
}

impl Hypervisor for MockHypervisor {
    fn backend(&self) -> Backend {
        Backend::Null
    }

    fn create_partition(&self, vcpus: u32, _memory_mib: u64) -> AvmResult<Box<dyn Partition>> {
        Ok(Box::new(MockPartition {
            script: self.script.clone(),
            vcpus,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_replays_then_halts() {
        let h = MockHypervisor::with_script(vec![VcpuExit::Halt, VcpuExit::Halt]);
        let mut p = h.create_partition(1, 64).unwrap();
        assert_eq!(p.run_vcpu(0).unwrap(), VcpuExit::Halt);
        assert_eq!(p.run_vcpu(0).unwrap(), VcpuExit::Halt);
        assert_eq!(p.run_vcpu(0).unwrap(), VcpuExit::Halt); // 耗尽后默认 Halt
    }

    #[test]
    fn vcpu_range_enforced() {
        let h = MockHypervisor::default();
        let mut p = h.create_partition(1, 64).unwrap();
        assert!(p.run_vcpu(1).is_err());
    }
}
