//! PCI 总线模型（M2）。
//!
//! 实现最小 Type-0 配置空间 + 设备挂载 + 配置读写路由（对应 host 的
//! 0xCF8/0xCFC 索引/数据端口语义）；MMIO BAR 路由在接入 vcpu 循环时扩展。

use avm_core::{AvmError, AvmResult};

/// PCI 设备在总线上的最小接口。
pub trait PciDevice: Send {
    fn vendor_id(&self) -> u16;
    fn device_id(&self) -> u16;
    /// 设备类代码（class code 高字节，如 0x01 = mass storage）。
    fn class_code(&self) -> u8;
    /// MMIO BAR（BAR0）区域大小；0 = 无 BAR。
    fn bar0_size(&self) -> u32;
    /// 设备收到 BAR0 MMIO 读写。
    fn bar0_read(&self, offset: u64, size: u8) -> AvmResult<u32>;
    fn bar0_write(&mut self, offset: u64, size: u8, value: u32) -> AvmResult<()>;
    /// 下转型支持（DeviceSet 取回具体设备）。
    fn as_any(&self) -> &dyn std::any::Any;
}

/// 配置空间寄存器偏移（Type-0 头）。
pub mod cfg {
    pub const VENDOR_ID: usize = 0x00;
    pub const DEVICE_ID: usize = 0x02;
    pub const COMMAND: usize = 0x04;
    pub const STATUS: usize = 0x06;
    pub const CLASS_CODE: usize = 0x08;
    pub const HEADER_TYPE: usize = 0x0E;
    pub const BAR0: usize = 0x10;
}

/// 总线上的一个设备槽位。
pub struct PciSlot {
    pub device: Box<dyn PciDevice>,
    /// 分配的 BAR0 基址（4KiB 对齐）；0 = 未分配。
    pub bar0_base: u64,
}

/// 最小 PCI 总线：设备挂载 + 配置空间读 + BAR0 MMIO 路由。
#[derive(Default)]
pub struct PciBus {
    slots: Vec<PciSlot>,
}

impl PciBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 挂载设备，自动分配 BAR0 基址（4KiB 对齐，从 0xF000_0000 向下生长）。
    pub fn attach(&mut self, device: Box<dyn PciDevice>) -> AvmResult<usize> {
        let size = device.bar0_size();
        if size == 0 || size % 4096 != 0 {
            return Err(AvmError::Device(format!(
                "BAR0 大小非法（{size}）：须为 4KiB 整数倍且非零"
            )));
        }
        // 从末尾向前找第一个未冲突的 4KiB 对齐基址
        let next_base = self
            .slots
            .iter()
            .map(|s| s.bar0_base + s.device.bar0_size() as u64)
            .max()
            .unwrap_or(0xF000_0000);
        self.slots.push(PciSlot {
            device,
            bar0_base: next_base,
        });
        Ok(self.slots.len() - 1)
    }

    pub fn devices(&self) -> &[PciSlot] {
        &self.slots
    }

    /// 配置空间读：4 对齐 offset 走 32 位读，2 对齐 offset 走 16 位读。
    /// 返回值低字节序与 PCI 小端一致（调用方按目标宽度截取）。
    pub fn config_read(&self, slot: usize, offset: usize, size: u8) -> AvmResult<u32> {
        let dev = self
            .slots
            .get(slot)
            .ok_or_else(|| AvmError::Device(format!("PCI 槽位 {slot} 不存在")))?;
        if size != 1 && size != 2 && size != 4 {
            return Err(AvmError::Device(format!("配置空间读宽度非法：{size}")));
        }
        if offset + size as usize > 256 || (offset as u64) % (size as u64) != 0 {
            return Err(AvmError::Device(format!(
                "配置空间读非法：offset=0x{offset:02x} size={size}"
            )));
        }
        let mut cfg_bytes = [0u8; 256];
        // Vendor/Device（0x00..0x04）
        cfg_bytes[cfg::VENDOR_ID..cfg::VENDOR_ID + 2]
            .copy_from_slice(&dev.device.vendor_id().to_le_bytes());
        cfg_bytes[cfg::DEVICE_ID..cfg::DEVICE_ID + 2]
            .copy_from_slice(&dev.device.device_id().to_le_bytes());
        // Command/Status
        cfg_bytes[cfg::COMMAND..cfg::COMMAND + 2].copy_from_slice(&0x0007u16.to_le_bytes());
        cfg_bytes[cfg::STATUS..cfg::STATUS + 2].copy_from_slice(&0x0010u16.to_le_bytes());
        // Class Code（0x08..0x0C）：rev | prog_if | subclass | base class
        cfg_bytes[cfg::CLASS_CODE] = 0; // revision
        cfg_bytes[cfg::CLASS_CODE + 1] = 0; // prog_if
        cfg_bytes[cfg::CLASS_CODE + 2] = 0; // subclass
        cfg_bytes[cfg::CLASS_CODE + 3] = dev.device.class_code(); // base class
                                                                  // Header Type（0x0E）= Type-0
        cfg_bytes[cfg::HEADER_TYPE] = 0;
        // BAR0（0x10..0x14）：32 位 MMIO、非 prefetchable
        let bar0 = ((dev.bar0_base as u32) & 0xFFFF_F000) | 0x0;
        cfg_bytes[cfg::BAR0..cfg::BAR0 + 4].copy_from_slice(&bar0.to_le_bytes());

        let raw = &cfg_bytes[offset..offset + size as usize];
        let mut v = 0u32;
        for (i, b) in raw.iter().enumerate() {
            v |= (*b as u32) << (i * 8);
        }
        Ok(v)
    }

    /// 按 BAR0 基址路由 MMIO 读。
    pub fn mmio_read(&self, addr: u64, size: u8) -> AvmResult<u32> {
        for s in &self.slots {
            let size64 = s.device.bar0_size() as u64;
            if addr >= s.bar0_base && addr < s.bar0_base + size64 {
                return s.device.bar0_read(addr - s.bar0_base, size);
            }
        }
        Err(AvmError::Device(format!("MMIO 读无路由：0x{addr:08x}")))
    }

    /// 按 BAR0 基址路由 MMIO 写。
    pub fn mmio_write(&mut self, addr: u64, size: u8, value: u32) -> AvmResult<()> {
        let idx = self
            .slots
            .iter()
            .position(|s| {
                let size64 = s.device.bar0_size() as u64;
                addr >= s.bar0_base && addr < s.bar0_base + size64
            })
            .ok_or_else(|| AvmError::Device(format!("MMIO 写无路由：0x{addr:08x}")))?;
        let base = self.slots[idx].bar0_base;
        self.slots[idx].device.bar0_write(addr - base, size, value)
    }
}

/// virtio-blk 的 PCI 配置（device_id 0x1001，class 0x01）。
pub const VIRTIO_BLK_DEVICE_ID: u16 = 0x1001;
pub const VIRTIO_VENDOR_ID: u16 = 0x1AF4;

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试桩设备：4KiB BAR0，寄存器为简单数组。
    struct FakeDev {
        regs: [u32; 4],
    }

    impl FakeDev {
        fn new() -> Self {
            Self { regs: [0; 4] }
        }
    }

    impl PciDevice for FakeDev {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn vendor_id(&self) -> u16 {
            VIRTIO_VENDOR_ID
        }
        fn device_id(&self) -> u16 {
            VIRTIO_BLK_DEVICE_ID
        }
        fn class_code(&self) -> u8 {
            0x01
        }
        fn bar0_size(&self) -> u32 {
            4096
        }
        fn bar0_read(&self, offset: u64, _size: u8) -> AvmResult<u32> {
            self.regs
                .get(offset as usize / 4)
                .copied()
                .ok_or_else(|| AvmError::Device("BAR0 读越界".into()))
        }
        fn bar0_write(&mut self, offset: u64, _size: u8, value: u32) -> AvmResult<()> {
            let idx = offset as usize / 4;
            if idx >= 4 {
                return Err(AvmError::Device("BAR0 写越界".into()));
            }
            self.regs[idx] = value;
            Ok(())
        }
    }

    #[test]
    fn attach_allocates_aligned_bars() {
        let mut bus = PciBus::new();
        let slot0 = bus.attach(Box::new(FakeDev::new())).unwrap();
        let slot1 = bus.attach(Box::new(FakeDev::new())).unwrap();
        assert_ne!(slot0, slot1);
        let bases: Vec<u64> = bus.devices().iter().map(|s| s.bar0_base).collect();
        assert_eq!(bases[0] % 4096, 0);
        assert_eq!(bases[1] % 4096, 0);
        // 无重叠
        assert!(bases[1] >= bases[0] + 4096);
    }

    #[test]
    fn attach_rejects_bad_bar_size() {
        struct BadDev;
        impl PciDevice for BadDev {
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn vendor_id(&self) -> u16 {
                0
            }
            fn device_id(&self) -> u16 {
                0
            }
            fn class_code(&self) -> u8 {
                0
            }
            fn bar0_size(&self) -> u32 {
                100 // 非 4KiB 倍数
            }
            fn bar0_read(&self, _: u64, _: u8) -> AvmResult<u32> {
                Ok(0)
            }
            fn bar0_write(&mut self, _: u64, _: u8, _: u32) -> AvmResult<()> {
                Ok(())
            }
        }
        assert!(PciBus::new().attach(Box::new(BadDev)).is_err());
    }

    #[test]
    fn config_space_reads() {
        let mut bus = PciBus::new();
        bus.attach(Box::new(FakeDev::new())).unwrap();
        assert_eq!(bus.config_read(0, cfg::VENDOR_ID, 2).unwrap(), 0x1AF4);
        assert_eq!(bus.config_read(0, cfg::DEVICE_ID, 2).unwrap(), 0x1001);
        // 32 位读合并 vendor+device
        assert_eq!(bus.config_read(0, 0x00, 4).unwrap(), 0x1001_1AF4);
        assert_eq!(bus.config_read(0, cfg::CLASS_CODE, 4).unwrap(), 0x0100_0000);
        assert_eq!(bus.config_read(0, cfg::HEADER_TYPE, 1).unwrap(), 0);
        let bar = bus.config_read(0, cfg::BAR0, 4).unwrap();
        assert_eq!(bar & 0xF, 0); // 32 位 MMIO
                                  // 宽度非法/越界/不存在；1 字节读任意偏移合法
        assert!(bus.config_read(0, 0x03, 1).is_ok());
        assert!(bus.config_read(0, 0x02, 1).is_ok());
        assert!(bus.config_read(0, 0x00, 3).is_err());
        assert!(bus.config_read(0, 0xFE, 4).is_err());
        assert!(bus.config_read(5, cfg::VENDOR_ID, 2).is_err());
    }

    #[test]
    fn mmio_routes_to_device() {
        let mut bus = PciBus::new();
        bus.attach(Box::new(FakeDev::new())).unwrap();
        let base = bus.devices()[0].bar0_base;
        bus.mmio_write(base + 8, 4, 0xDEAD_BEEF).unwrap();
        assert_eq!(bus.mmio_read(base + 8, 4).unwrap(), 0xDEAD_BEEF);
        // 越界路由
        assert!(bus.mmio_read(base + 4096, 4).is_err());
        assert!(bus.mmio_read(0x1000, 4).is_err());
        // 写越界传播设备错误
        assert!(bus.mmio_write(base + 4060, 4, 1).is_err());
    }
}
