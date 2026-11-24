//! avm-hypervisor：Hypervisor 抽象 trait + WHPX 实现（M0）。
//!
//! 分层契约：vmm 只依赖本 crate 的 `Hypervisor` trait；
//! 平台实现（whp / kvm / hvf）各自独立模块，编译期 feature 选择。

pub mod doctor;
pub mod whp;

use avm_core::{AvmError, AvmResult};

/// 平台后端标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    WHPX,
    KVM,
    HVF,
    Null, // 骨架桩，仅用于编译验证与单元测试
}

/// 一个已创建的最小分区句柄（M0：toy guest 只需 CPU + 内存 + 串口）。
pub trait Partition: Send {
    /// 映射 guest 物理内存区域。
    fn map_memory(&mut self, gpa: u64, host: &mut [u8]) -> AvmResult<()>;

    /// 启动指定 vcpu 执行，直到退出（exit reason 由实现返回）。
    fn run_vcpu(&mut self, vcpu: u32) -> AvmResult<VcpuExit>;

    /// 设置 vcpu 寄存器组（引导初期设置 RIP/RSP/模式）。
    fn set_regs(&mut self, vcpu: u32, rip: u64, rsp: u64) -> AvmResult<()>;
}

/// vcpu 退出原因的最小集合（随里程碑扩展）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcpuExit {
    Halt,
    Io { port: u16, size: u8, data: u32 },
    Mmio { gpa: u64, size: u8 },
    Unknown(u32),
}

/// Hypervisor 工厂 trait：同一入口在 M8 后按平台分发。
pub trait Hypervisor: Send {
    fn backend(&self) -> Backend;
    fn create_partition(&self, vcpus: u32, memory_mib: u64) -> AvmResult<Box<dyn Partition>>;
}

/// 骨架阶段可用的空实现，保证 workspace 可编译、可测试。
pub struct NullHypervisor;

impl Hypervisor for NullHypervisor {
    fn backend(&self) -> Backend {
        Backend::Null
    }

    fn create_partition(&self, _vcpus: u32, _memory_mib: u64) -> AvmResult<Box<dyn Partition>> {
        Err(AvmError::Hypervisor(
            "NullHypervisor: 仅骨架桩，等待 M0 WHPX 实现".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_backend_reports_null() {
        assert_eq!(NullHypervisor.backend(), Backend::Null);
    }

    #[test]
    fn null_create_partition_errors() {
        let h = NullHypervisor;
        assert!(h.create_partition(1, 64).is_err());
    }
}
