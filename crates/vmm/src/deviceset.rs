//! 设备编排（M5）：把 virtio-blk/net/gpu 挂上 PCI 总线，组成 Android 引导所需的设备集。

use crate::bus::PciBus;
use crate::virtio_pci::{VirtioBlkPci, VirtioGpuPci, VirtioNetPci};
use avm_core::{AvmError, AvmResult};
use avm_devices::virtio_blk::VirtioBlk;
use avm_devices::virtio_gpu::VirtioGpu;
use avm_devices::virtio_net::NetBackend;

/// Android 默认磁盘容量（MiB）——Android x86 rootfs 最小可用规模。
pub const ANDROID_DISK_MIB: u64 = 2048;
/// Android 默认 MAC（本地管理位、静态分配段）。
pub const ANDROID_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x41, 0x56, 0x4D];

/// 一套挂在 PCI 总线上的设备（blk + net + gpu）。
pub struct DeviceSet {
    /// PCI 总线（已挂载全部设备）。
    pub bus: PciBus,
    /// virtio-blk 槽位。
    pub blk_slot: usize,
    /// virtio-net 槽位。
    pub net_slot: usize,
    /// virtio-gpu 槽位。
    pub gpu_slot: usize,
}

impl DeviceSet {
    /// 挂载设备集。`net` 后端由调用方提供（TAP / 回环）。
    pub fn attach_all(
        disk_mib: u64,
        mac: [u8; 6],
        net: Box<dyn NetBackend>,
    ) -> AvmResult<DeviceSet> {
        let mut bus = PciBus::new();
        let blk_slot = bus.attach(Box::new(VirtioBlkPci::new(disk_mib)))?;
        let net_slot = bus.attach(Box::new(VirtioNetPci::new(net, mac)))?;
        let gpu_slot = bus.attach(Box::new(VirtioGpuPci::new()))?;
        Ok(DeviceSet {
            bus,
            blk_slot,
            net_slot,
            gpu_slot,
        })
    }

    /// Android 默认设备集（2GiB 盘、固定 MAC）。
    pub fn android_default(net: Box<dyn NetBackend>) -> AvmResult<DeviceSet> {
        Self::attach_all(ANDROID_DISK_MIB, ANDROID_MAC, net)
    }

    /// 直接借用 GPU 设备（截图出口）。
    pub fn gpu(&self) -> AvmResult<&VirtioGpu> {
        let slot = self
            .bus
            .devices()
            .get(self.gpu_slot)
            .ok_or_else(|| AvmError::Device("gpu 槽位不存在".into()))?;
        slot.device
            .as_any()
            .downcast_ref::<VirtioGpuPci>()
            .map(|p| p.gpu())
            .ok_or_else(|| AvmError::Device("槽位不是 virtio-gpu".into()))
    }

    /// 直接借用 blk 设备（容量校验）。
    pub fn blk(&self) -> AvmResult<&VirtioBlk> {
        let slot = self
            .bus
            .devices()
            .get(self.blk_slot)
            .ok_or_else(|| AvmError::Device("blk 槽位不存在".into()))?;
        slot.device
            .as_any()
            .downcast_ref::<VirtioBlkPci>()
            .map(|p| p.blk())
            .ok_or_else(|| AvmError::Device("槽位不是 virtio-blk".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::cfg;
    use avm_devices::virtio_net::LoopbackBackend;

    #[test]
    fn android_default_attaches_three_devices() {
        let ds = DeviceSet::android_default(Box::new(LoopbackBackend::new())).unwrap();
        assert_eq!(ds.bus.devices().len(), 3);
        // 三个槽位的 PCI ID
        assert_eq!(
            ds.bus.config_read(ds.blk_slot, cfg::DEVICE_ID, 2).unwrap(),
            0x1001
        );
        assert_eq!(
            ds.bus.config_read(ds.net_slot, cfg::DEVICE_ID, 2).unwrap(),
            0x1000
        );
        assert_eq!(
            ds.bus.config_read(ds.gpu_slot, cfg::DEVICE_ID, 2).unwrap(),
            0x1010
        );
    }

    #[test]
    fn typed_accessors_reach_devices() {
        let ds = DeviceSet::android_default(Box::new(LoopbackBackend::new())).unwrap();
        // blk：Android 默认 2GiB = 4MiB 扇区数
        let blk = ds.blk().unwrap();
        assert_eq!(blk.capacity_sectors(), ANDROID_DISK_MIB * 1024 * 1024 / 512);
        // gpu：帧缓冲长度 = 800*600*4
        let gpu = ds.gpu().unwrap();
        assert_eq!(
            gpu.framebuffer().len(),
            800 * 600 * avm_devices::virtio_gpu::PIXEL_SIZE
        );
    }

    #[test]
    fn wrong_slot_type_rejected() {
        // blk() 拿 gpu 槽位 → 报错（反向同理）
        let ds = DeviceSet::android_default(Box::new(LoopbackBackend::new())).unwrap();
        let swapped = DeviceSet {
            bus: ds.bus,
            blk_slot: ds.gpu_slot,
            net_slot: ds.net_slot,
            gpu_slot: ds.blk_slot,
        };
        assert!(swapped.blk().is_err());
        assert!(swapped.gpu().is_err());
    }
}
