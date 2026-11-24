//! avm-vmm：builder / boot / bus / vcpu 编排层。
//!
//! 组合 hypervisor 与 devices，负责内存布局、内核加载（linux-loader）、
//! ACPI 表生成（acpi-tables）、virtio 总线挂载与 vcpu 调度循环。

pub mod acpi;
pub mod boot;
pub mod bus;
pub mod mock;
pub mod toy;
pub mod virtio_pci;

use crate::toy::{SERIAL_PORT, TOY_ENTRY_RIP, TOY_LOAD_GPA};
use avm_core::{AvmError, AvmResult, InstanceConfig, InstanceState};
use avm_devices::{BusDevice, SerialUart};
use avm_hypervisor::{Hypervisor, VcpuExit};

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
            _config: self.config.clone(),
        })
    }
}

/// 一台已构建（未必已引导）的 VM。
#[derive(Debug)]
pub struct VmInstance {
    pub state: InstanceState,
    _config: InstanceConfig,
}

impl VmInstance {
    /// M1：加载内核与 initrd，设置长模式入口（真机执行待 WHPX 验证）。
    ///
    /// 返回 vcpu 初始寄存器；实际 `run_vcpu` 由 daemon/CLI 层驱动。
    pub fn boot(
        &mut self,
        hv: &dyn Hypervisor,
        assets: &boot::BootAssets,
    ) -> AvmResult<boot::VcpuBootRegs> {
        if self.config().memory_mib < boot::layout::MIN_MEMORY_MIB {
            return Err(AvmError::Config(format!(
                "Linux 引导至少需要 {} MiB 内存",
                boot::layout::MIN_MEMORY_MIB
            )));
        }
        let loaded = boot::load_kernel(self.config().memory_mib, assets)?;
        let regs = boot::boot_regs(&loaded.kernel)?;
        self.state = InstanceState::Booting;
        let _ = (hv, loaded); // 分区与 guest 内存由 daemon 层持有（M6 收口）
        Ok(regs)
    }

    pub fn config(&self) -> &InstanceConfig {
        &self._config
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

/// toy guest vcpu 循环的最大退出次数（防止失控）。
const TOY_MAX_EXITS: usize = 4096;

/// M0 引导 toy guest：加载 0x7c00 → 设置 RIP → vcpu 循环处理 IO exit。
///
/// 串口（0x3F8）写入转发给 `SerialUart`（host stdout），并收集返回，
/// 供测试与 `avm start --toy` 输出比对。
pub fn boot_toy_guest(hv: &dyn Hypervisor) -> AvmResult<Vec<u8>> {
    let partition = hv.create_partition(1, 64)?;

    let mut partition = partition;
    let mut image = toy::toy_boot_sector();
    partition.map_memory(TOY_LOAD_GPA, &mut image)?;
    partition.set_regs(0, TOY_ENTRY_RIP, 0)?;

    let mut uart = SerialUart;
    let mut written: Vec<u8> = Vec::new();

    for _ in 0..TOY_MAX_EXITS {
        match partition.run_vcpu(0)? {
            VcpuExit::Io { port, size, data } => {
                if port != SERIAL_PORT {
                    return Err(AvmError::Boot(format!(
                        "toy guest 访问了未实现的 IO 端口 0x{port:04x}"
                    )));
                }
                let byte = (data & 0xff) as u8;
                uart.write(port as u64, size, byte as u64)?;
                written.push(byte);
            }
            VcpuExit::Halt => {
                return Ok(written);
            }
            other => {
                return Err(AvmError::Boot(format!("toy guest 意外退出：{other:?}")));
            }
        }
    }

    Err(AvmError::Boot(format!(
        "toy guest 超过 {TOY_MAX_EXITS} 次退出仍未 halt"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockHypervisor;
    use crate::toy::toy_message;
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
        assert!(matches!(b.build(&NullHypervisor), Err(AvmError::Config(_))));
    }

    #[test]
    fn rejects_too_few_memory() {
        let b = VmmBuilder::new(cfg("vm0", 2, 256));
        assert!(matches!(b.build(&NullHypervisor), Err(AvmError::Config(_))));
    }

    #[test]
    fn toy_guest_prints_message_via_serial() {
        // 模拟真实执行：guest 逐字符 OUT 到串口后 hlt
        let script = toy_message()
            .bytes()
            .map(|b| VcpuExit::Io {
                port: SERIAL_PORT,
                size: 1,
                data: b as u32,
            })
            .chain([VcpuExit::Halt])
            .collect();
        let hv = MockHypervisor::with_script(script);
        let out = boot_toy_guest(&hv).unwrap();
        assert_eq!(out, toy_message().as_bytes());
    }

    #[test]
    fn toy_guest_rejects_foreign_port() {
        let script = vec![
            VcpuExit::Io {
                port: 0x60,
                size: 1,
                data: 0x41,
            },
            VcpuExit::Halt,
        ];
        let hv = MockHypervisor::with_script(script);
        assert!(matches!(boot_toy_guest(&hv), Err(AvmError::Boot(_))));
    }

    #[test]
    fn toy_guest_rejects_runaway() {
        // 无 Halt：脚本耗尽后 mock 默认回 Halt，因此改用意外退出路径验证
        let script = vec![VcpuExit::Mmio { gpa: 0, size: 1 }];
        let hv = MockHypervisor::with_script(script);
        assert!(matches!(boot_toy_guest(&hv), Err(AvmError::Boot(_))));
    }
}
