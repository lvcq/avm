//! PCI 适配器：把 avm-devices 的 virtio 设备接到 PCI 总线（M2）。
//!
//! virtio-blk（0x1AF4:0x1001）作为 Type-0 设备挂载，BAR0（4KiB MMIO）布局：
//!   0x000..0x0FF  预留（M2+ 接 virtio common config）
//!   0x100..0x108  device config：容量（扇区数，LE u64）
//!   0x108..0x200  notify 区域（写 = 通知设备处理队列）

use crate::bus::{PciBus, PciDevice};
use avm_core::{AvmError, AvmResult};
use avm_devices::virtio_blk::VirtioBlk;
use avm_devices::virtio_gpu::VirtioGpu;
use avm_devices::virtio_net::{NetBackend, VirtioNet};

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

/// virtio-gpu PCI 适配器（0x1AF4:0x1010，显示类 0x03）。
/// BAR0 布局：0x000..0x0FF 预留；0x100..0x200 notify 区（M4a 无 device config）。
pub struct VirtioGpuPci {
    gpu: VirtioGpu,
}

impl VirtioGpuPci {
    pub fn new() -> Self {
        Self {
            gpu: VirtioGpu::new(),
        }
    }

    pub fn gpu(&self) -> &VirtioGpu {
        &self.gpu
    }

    pub fn gpu_mut(&mut self) -> &mut VirtioGpu {
        &mut self.gpu
    }
}

impl Default for VirtioGpuPci {
    fn default() -> Self {
        Self::new()
    }
}

impl PciDevice for VirtioGpuPci {
    fn vendor_id(&self) -> u16 {
        0x1AF4
    }

    fn device_id(&self) -> u16 {
        0x1010
    }

    fn class_code(&self) -> u8 {
        0x03 // 显示控制器
    }

    fn bar0_size(&self) -> u32 {
        4096
    }

    fn bar0_read(&self, _offset: u64, _size: u8) -> AvmResult<u32> {
        Err(AvmError::Device(
            "virtio-gpu BAR0 读未实现（M4a 无 device config）".into(),
        ))
    }

    fn bar0_write(&mut self, offset: u64, size: u8, _value: u32) -> AvmResult<()> {
        if !(0x100..0x200).contains(&offset) {
            return Err(AvmError::Device(format!(
                "BAR0 写越界/未实现：offset=0x{offset:03x}"
            )));
        }
        let _ = size;
        Ok(()) // notify：M4b+ 在此触发 process_ctrl_queue
    }
}

/// virtio-net PCI 适配器（0x1AF4:0x1000，网络类 0x02）。
/// BAR0 布局：0x000..0x0FF 预留；0x100..0x107 device config（MAC 6B + 链路状态 1B）；
/// 0x108..0x200 notify 区。
pub struct VirtioNetPci {
    net: VirtioNet,
}

impl VirtioNetPci {
    pub fn new(backend: Box<dyn NetBackend>, mac: [u8; 6]) -> Self {
        Self {
            net: VirtioNet::new(mac, backend),
        }
    }

    pub fn net(&self) -> &VirtioNet {
        &self.net
    }

    pub fn net_mut(&mut self) -> &mut VirtioNet {
        &mut self.net
    }
}

impl PciDevice for VirtioNetPci {
    fn vendor_id(&self) -> u16 {
        0x1AF4
    }

    fn device_id(&self) -> u16 {
        0x1000
    }

    fn class_code(&self) -> u8 {
        0x02 // 网络控制器
    }

    fn bar0_size(&self) -> u32 {
        4096
    }

    fn bar0_read(&self, offset: u64, size: u8) -> AvmResult<u32> {
        if !(0x100..0x107).contains(&offset) {
            return Err(AvmError::Device(format!(
                "BAR0 读越界/未实现：offset=0x{offset:03x}"
            )));
        }
        if size != 1 {
            return Err(AvmError::Device("device config 仅支持 1 字节读".into()));
        }
        let mac = self.net.config_mac();
        let mut cfg = [0u8; 7];
        cfg[..6].copy_from_slice(&mac);
        cfg[6] = self.net.config_link_status();
        Ok(cfg[(offset - 0x100) as usize] as u32)
    }

    fn bar0_write(&mut self, offset: u64, size: u8, _value: u32) -> AvmResult<()> {
        if !(0x108..0x200).contains(&offset) {
            return Err(AvmError::Device(format!(
                "BAR0 写越界/未实现：offset=0x{offset:03x}"
            )));
        }
        let _ = size;
        Ok(()) // notify：M3+ 在此触发 process_rx/process_tx
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
    use avm_devices::virtio_net::{LoopbackBackend, VIRTIO_NET_S_LINK_UP};

    #[test]
    fn gpu_pci_identity() {
        let mut bus = PciBus::new();
        let slot = bus.attach(Box::new(VirtioGpuPci::new())).unwrap();
        assert_eq!(bus.config_read(slot, cfg::VENDOR_ID, 2).unwrap(), 0x1AF4);
        assert_eq!(bus.config_read(slot, cfg::DEVICE_ID, 2).unwrap(), 0x1010);
        assert_eq!(
            bus.config_read(slot, cfg::CLASS_CODE, 4).unwrap(),
            0x0300_0000
        );
        let base = bus.devices()[slot].bar0_base;
        assert!(bus.mmio_write(base + 0x110, 4, 0).is_ok()); // notify 区
        assert!(bus.mmio_read(base + 0x110, 1).is_err()); // 读未实现
    }

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

    fn build_net_bus() -> (PciBus, usize) {
        let mut bus = PciBus::new();
        let slot = bus
            .attach(Box::new(VirtioNetPci::new(
                Box::new(LoopbackBackend::new()),
                [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            )))
            .unwrap();
        (bus, slot)
    }

    #[test]
    fn net_pci_identity() {
        let (bus, slot) = build_net_bus();
        assert_eq!(bus.config_read(slot, cfg::VENDOR_ID, 2).unwrap(), 0x1AF4);
        assert_eq!(bus.config_read(slot, cfg::DEVICE_ID, 2).unwrap(), 0x1000);
        assert_eq!(
            bus.config_read(slot, cfg::CLASS_CODE, 4).unwrap(),
            0x0200_0000
        );
    }

    #[test]
    fn net_mac_and_link_readable_via_mmio() {
        let (mut bus, slot) = build_net_bus();
        let base = bus.devices()[slot].bar0_base;
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        for (i, b) in mac.iter().enumerate() {
            assert_eq!(
                bus.mmio_read(base + 0x100 + i as u64, 1).unwrap(),
                *b as u32
            );
        }
        assert_eq!(
            bus.mmio_read(base + 0x106, 1).unwrap(),
            VIRTIO_NET_S_LINK_UP as u32
        );
        // 越界读被拒
        assert!(bus.mmio_read(base + 0x107, 1).is_err());
        // notify 区写 OK
        assert!(bus.mmio_write(base + 0x110, 4, 0).is_ok());
    }
}
