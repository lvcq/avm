//! avm-devices：virtio-blk / net / gpu / input / UART 设备模型。
//!
//! 队列管理使用 virtio-queue，规范常量来自 virtio-bindings。
//! 设备通过统一的 `BusDevice` trait 挂载到 vmm 的总线（M2 PCI）。

use avm_core::{AvmError, AvmResult};

/// 设备在总线上的最小接口（M2 起扩展 PCI BAR 映射）。
pub trait BusDevice: Send {
    fn id(&self) -> &'static str;
    /// MMIO/PIO 读写入口。
    fn read(&self, offset: u64, size: u8) -> AvmResult<u64>;
    fn write(&mut self, offset: u64, size: u8, value: u64) -> AvmResult<()>;
}

pub mod virtio_blk;

/// 16550 串口：M0/M1 引导输出通道（toy guest 的打印出口）。
#[derive(Debug, Default)]
pub struct SerialUart;

impl BusDevice for SerialUart {
    fn id(&self) -> &'static str {
        "uart-16550"
    }

    fn read(&self, _offset: u64, _size: u8) -> AvmResult<u64> {
        Ok(0)
    }

    fn write(&mut self, _offset: u64, _size: u8, value: u64) -> AvmResult<()> {
        // M0: value 即 guest 写入的字符，输出到 host stdout
        if let Some(ch) = char::from_u32(value as u32) {
            print!("{ch}");
        }
        Ok(())
    }
}

/// virtio-blk 骨架：M2 实现队列与块设备后端。
#[derive(Debug, Default)]
pub struct VirtioBlk {
    pub image_path: String,
}

impl BusDevice for VirtioBlk {
    fn id(&self) -> &'static str {
        "virtio-blk"
    }

    fn read(&self, _offset: u64, _size: u8) -> AvmResult<u64> {
        Err(AvmError::Device("virtio-blk: M2 实现".into()))
    }

    fn write(&mut self, _offset: u64, _size: u8, _value: u64) -> AvmResult<()> {
        Err(AvmError::Device("virtio-blk: M2 实现".into()))
    }
}

/// virtio-net 骨架：M3 实现（目标：adb 可达）。
#[derive(Debug, Default)]
pub struct VirtioNet;

impl BusDevice for VirtioNet {
    fn id(&self) -> &'static str {
        "virtio-net"
    }

    fn read(&self, _offset: u64, _size: u8) -> AvmResult<u64> {
        Err(AvmError::Device("virtio-net: M3 实现".into()))
    }

    fn write(&mut self, _offset: u64, _size: u8, _value: u64) -> AvmResult<()> {
        Err(AvmError::Device("virtio-net: M3 实现".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_echoes_writes() {
        let mut uart = SerialUart::default();
        assert_eq!(uart.id(), "uart-16550");
        assert!(uart.write(0, 1, b'M' as u64).is_ok());
    }

    #[test]
    fn unimplemented_devices_error() {
        let blk = VirtioBlk::default();
        assert!(blk.read(0, 4).is_err());
    }
}
pub mod bmp;
pub mod display;
pub mod virtio_gpu;
pub mod virtio_net;
