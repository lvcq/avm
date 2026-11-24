//! PCI 适配器：把 avm-devices 的 virtio 设备接到 PCI 总线（M2）。
//!
//! virtio-blk（0x1AF4:0x1001）作为 Type-0 设备挂载，BAR0（4KiB MMIO）布局：
//!   0x000..0x0FF  预留（M2+ 接 virtio common config）
//!   0x100..0x108  device config：容量（扇区数，LE u64）
//!   0x108..0x200  notify 区域（写 = 通知设备处理队列）

use crate::bus::{PciBus, PciDevice};
use avm_core::{AvmError, AvmResult};
use avm_devices::virtio_blk::VirtioBlk;

/// virtio-blk PCI 适配器。
pub struct VirtioBlkPci {
    blk: VirtioBlk,
}

impl VirtioBlkPci {
    pub fn new(capacity_mib: u64) -> Self {
        Self {
            blk: VirtioBlk::new(capacity_mib),
        }
    }

    pub fn blk(&self) -> &VirtioBlk {
        &self.blk
    }

    pub fn blk_mut(&mut self) -> &mut VirtioBlk {
        &mut self.blk
    }
}

impl PciDevice for VirtioBlkPci {
    fn vendor_id(&self) -> u16 {
        0x1AF4
    }

    fn device_id(&self) -> u16 {
        0x1001
    }

    fn class_code(&self) -> u8 {
        0x01
    }

    fn bar0_size(&self) -> u32 {
        4096
    }

    fn bar0_read(&self, offset: u64, size: u8) -> AvmResult<u32> {
        if !(0x100..0x108).contains(&offset) {
            return Err(AvmError::Device(format!(
                "BAR0 读越界/未实现：offset=0x{offset:03x}"
            )));
        }
        if size != 1 {
            return Err(AvmError::Device("device config 仅支持 1 字节读".into()));
        }
        let cap = self.blk.config_capacity();
        Ok(cap[(offset - 0x100) as usize] as u32)
    }

    fn bar0_write(&mut self, offset: u64, size: u8, _value: u32) -> AvmResult<()> {
        if !(0x108..0x200).contains(&offset) {
            return Err(AvmError::Device(format!(
                "BAR0 写越界/未实现：offset=0x{offset:03x}"
            )));
        }
        let _ = size;
        Ok(()) // notify：M3+ 在此触发 process_queue
    }
}

/// 构建带 virtio-blk 的 PCI 总线，返回（总线，槽位号）。
pub fn build_default_bus(capacity_mib: u64) -> AvmResult<(PciBus, usize)> {
    let mut bus = PciBus::new();
    let slot = bus.attach(Box::new(VirtioBlkPci::new(capacity_mib)))?;
    Ok((bus, slot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::cfg;

    #[test]
    fn blk_pci_identity() {
        let (bus, slot) = build_default_bus(2).unwrap();
        assert_eq!(bus.config_read(slot, cfg::VENDOR_ID, 2).unwrap(), 0x1AF4);
        assert_eq!(bus.config_read(slot, cfg::DEVICE_ID, 2).unwrap(), 0x1001);
        assert_eq!(
            bus.config_read(slot, cfg::CLASS_CODE, 4).unwrap(),
            0x0100_0000
        );
    }

    #[test]
    fn capacity_readable_via_mmio() {
        let (bus, slot) = build_default_bus(2).unwrap();
        let base = bus.devices()[slot].bar0_base;
        let cap_bytes = 2u64 * 1024 * 1024 / 512; // 4096 扇区
        for i in 0..8u64 {
            let v = bus.mmio_read(base + 0x100 + i, 1).unwrap();
            assert_eq!(v as u8, ((cap_bytes >> (i * 8)) & 0xff) as u8);
        }
    }

    #[test]
    fn notify_region_writes_ok() {
        let (mut bus, slot) = build_default_bus(1).unwrap();
        let base = bus.devices()[slot].bar0_base;
        assert!(bus.mmio_write(base + 0x110, 4, 0).is_ok());
        // 读 notify 区未实现
        assert!(bus.mmio_read(base + 0x110, 1).is_err());
    }

    #[test]
    fn reserved_region_rejected() {
        let (mut bus, slot) = build_default_bus(1).unwrap();
        let base = bus.devices()[slot].bar0_base;
        assert!(bus.mmio_read(base + 0x000, 4).is_err());
        assert!(bus.mmio_write(base + 0x050, 4, 0).is_err());
    }
}
