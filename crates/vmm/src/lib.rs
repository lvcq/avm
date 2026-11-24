//! avm-vmm：builder / boot / bus / vcpu 编排层。
//!
//! 组合 hypervisor 与 devices，负责内存布局、内核加载（linux-loader）、
//! ACPI 表生成（acpi-tables）、virtio 总线挂载与 vcpu 调度循环。

use avm_core::{AvmError, AvmResult, InstanceConfig, InstanceState};
use avm_hypervisor::Hypervisor;

/// VMM 构建器：M1 起逐步填充（内存布局 -> linux-loader -> bus 挂载 -> boot）。
pub struct VmmBuilder {
    config: InstanceConfig,
}

impl VmmBuilder {
    pub fn new(config: InstanceConfig) -> Self {
        Self { config }
    }

    /// 校验配置并构建 VM 实例（M1 实现真实引导）。
    pub fn build(&self, hv: &dyn Hypervisor) -> AvmResult<VmInstance> {
        if self.config.id.is_empty() {
            return Err(AvmError::Config("instance id 不能为空".into()));
        }
        if self.config.vcpus == 0 || self.config.vcpus > 32 {
            return Err(AvmError::Config(format!(
                "vcpus={} 超出 [1,32]",
                self.config.vcpus
            )));
        }
        if self.config.memory_mib < 512 {
            return Err(AvmError::Config("Android 至少需要 512MiB 内存".into()));
        }
        hv.create_partition(self.config.vcpus, self.config.memory_mib)?;
        Ok(VmInstance {
            state: InstanceState::Created,
        })
    }
}

/// 一台已构建（未必已引导）的 VM。
#[derive(Debug)]
pub struct VmInstance {
    pub state: InstanceState,
}

impl VmInstance {
    /// M1：加载内核与 initrd，进入 guest shell。
    pub fn boot(&mut self) -> AvmResult<()> {
        Err(AvmError::Boot("boot: M1 里程碑实现".into()))
    }

    /// M6：由 daemon 通过 API 调用。
    pub fn pause(&mut self) -> AvmResult<()> {
        self.state = InstanceState::Paused;
        Ok(())
    }

    pub fn resume(&mut self) -> AvmResult<()> {
        self.state = InstanceState::Running;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use avm_hypervisor::NullHypervisor;

    fn cfg(id: &str, vcpus: u32, mem: u64) -> InstanceConfig {
        InstanceConfig {
            id: id.into(),
            vcpus,
            memory_mib: mem,
            ..Default::default()
        }
    }

    #[test]
    fn rejects_empty_id() {
        let b = VmmBuilder::new(cfg("", 2, 2048));
        assert!(matches!(
            b.build(&NullHypervisor),
            Err(AvmError::Config(_))
        ));
    }

    #[test]
    fn rejects_too_few_memory() {
        let b = VmmBuilder::new(cfg("vm0", 2, 256));
        assert!(matches!(
            b.build(&NullHypervisor),
            Err(AvmError::Config(_))
        ));
    }
}
