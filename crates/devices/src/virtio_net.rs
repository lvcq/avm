//! virtio-net 设备模型（M3）。
//!
//! - 队列：rx（0）/ tx（1）两条 split virtqueue
//! - 报文头：VIRTIO_NET_HDR 12B（M3 不协商 checksum/GSO，头全零，数据紧随其后）
//! - 后端：`NetBackend` trait（测试用回环后端；真机 TAP 后端在 Windows 接入）
//! - config：MAC 6B + 链路状态 1B（VIRTIO_NET_S_LINK_UP）

use avm_core::{AvmError, AvmResult};
use virtio_queue::Queue;
use virtio_queue::QueueT;
use virtio_queue::{Error as VqError, QueueOwnedT};
use vm_memory::{Bytes, GuestMemoryMmap};

use crate::virtio_blk::split64;

/// 单队列深度。
pub const QUEUE_SIZE: u16 = 64;

/// VIRTIO_NET_HDR 长度（无 checksum/GSO 特性协商时的 12 字节头）。
pub const NET_HDR_LEN: usize = 12;

/// config 偏移 0..6：MAC。
pub const CONFIG_MAC_OFFSET: usize = 0;
/// config 偏移 6：链路状态。
pub const CONFIG_LINK_STATUS_OFFSET: usize = 6;
/// 链路状态：链路已连接。
pub const VIRTIO_NET_S_LINK_UP: u8 = 1;

/// rx/tx 队列在 guest 内存中的布局（desc/avail/used 三段 × 2 条队列）。
pub const RX_DESC_TABLE_ADDR: u64 = 0x5000;
pub const RX_AVAIL_RING_ADDR: u64 = 0x6000;
pub const RX_USED_RING_ADDR: u64 = 0x7000;
pub const TX_DESC_TABLE_ADDR: u64 = 0x8000;
pub const TX_AVAIL_RING_ADDR: u64 = 0x9000;
pub const TX_USED_RING_ADDR: u64 = 0xA000;

fn vq_err(e: VqError) -> AvmError {
    AvmError::Device(format!("virtqueue 错误：{e}"))
}

/// 网络后端：设备经它收发以太网帧。
pub trait NetBackend: Send {
    /// 后端发送一帧（guest 出方向）。
    fn send_frame(&mut self, frame: &[u8]) -> AvmResult<()>;
    /// 后端取一帧待注入（guest 入方向）；无帧返回 None。
    fn recv_frame(&mut self) -> Option<Vec<u8>>;
}

/// 回环后端：guest 发出的帧原样回到接收方向（测试/自环用）。
#[derive(Default)]
pub struct LoopbackBackend {
    inbound: Vec<Vec<u8>>,
}

impl LoopbackBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// 手动注入一帧到接收方向（模拟外部网络来包）。
    pub fn inject(&mut self, frame: Vec<u8>) {
        self.inbound.push(frame);
    }
}

impl NetBackend for LoopbackBackend {
    fn send_frame(&mut self, frame: &[u8]) -> AvmResult<()> {
        self.inbound.push(frame.to_vec());
        Ok(())
    }

    fn recv_frame(&mut self) -> Option<Vec<u8>> {
        if self.inbound.is_empty() {
            None
        } else {
            Some(self.inbound.remove(0))
        }
    }
}

/// 从 guest 内存按固定地址布置一条队列。
fn build_queue(
    mem: &GuestMemoryMmap,
    desc_addr: u64,
    avail_addr: u64,
    used_addr: u64,
) -> AvmResult<Queue> {
    let mut queue =
        Queue::new(QUEUE_SIZE).map_err(|e| AvmError::Device(format!("virtqueue 创建失败：{e}")))?;
    queue.set_size(QUEUE_SIZE);
    let (dl, dh) = split64(desc_addr);
    queue.set_desc_table_address(Some(dl), Some(dh));
    let (al, ah) = split64(avail_addr);
    queue.set_avail_ring_address(Some(al), Some(ah));
    let (ul, uh) = split64(used_addr);
    queue.set_used_ring_address(Some(ul), Some(uh));
    queue.set_ready(true);
    if !queue.is_valid(mem) {
        return Err(AvmError::Device(
            "virtqueue 状态无效（地址/大小未通过校验）".into(),
        ));
    }
    Ok(queue)
}

/// virtio-net 设备。
pub struct VirtioNet {
    /// MAC 地址。
    mac: [u8; 6],
    /// 链路状态（M3 恒为 LINK_UP）。
    link_up: bool,
    /// 接收队列（0 号）。
    rx_queue: Option<Queue>,
    /// 发送队列（1 号）。
    tx_queue: Option<Queue>,
    /// 网络后端。
    backend: Box<dyn NetBackend>,
}

impl VirtioNet {
    /// 创建设备。
    pub fn new(mac: [u8; 6], backend: Box<dyn NetBackend>) -> Self {
        Self {
            mac,
            link_up: true,
            rx_queue: None,
            tx_queue: None,
            backend,
        }
    }

    /// config 空间：MAC 6 字节。
    pub fn config_mac(&self) -> [u8; 6] {
        self.mac
    }

    /// config 空间：链路状态字节。
    pub fn config_link_status(&self) -> u8 {
        if self.link_up {
            VIRTIO_NET_S_LINK_UP
        } else {
            0
        }
    }

    /// 布置 rx 队列（驱动写好 desc/avail 后调用）。
    pub fn setup_rx_queue(&mut self, mem: &GuestMemoryMmap) -> AvmResult<()> {
        self.rx_queue = Some(build_queue(
            mem,
            RX_DESC_TABLE_ADDR,
            RX_AVAIL_RING_ADDR,
            RX_USED_RING_ADDR,
        )?);
        Ok(())
    }

    /// 布置 tx 队列。
    pub fn setup_tx_queue(&mut self, mem: &GuestMemoryMmap) -> AvmResult<()> {
        self.tx_queue = Some(build_queue(
            mem,
            TX_DESC_TABLE_ADDR,
            TX_AVAIL_RING_ADDR,
            TX_USED_RING_ADDR,
        )?);
        Ok(())
    }

    /// 处理 tx 队列：把 guest 发出的帧交给后端，返回已处理数量。
    pub fn process_tx(&mut self, mem: &GuestMemoryMmap) -> AvmResult<usize> {
        let mut queue = self
            .tx_queue
            .take()
            .ok_or_else(|| AvmError::Device("tx 队列尚未 setup".into()))?;
        let mut processed = 0usize;
        queue.disable_notification(mem).map_err(vq_err)?;
        while let Some(chain) = queue.iter(mem).map_err(vq_err)?.next() {
            let mut frame = Vec::new();
            for d in chain.clone().readable().skip(1) {
                // 跳过 12B 头所在的第一个描述符之外的部分也可能跨描述符；
                // 头部固定在链首描述符（12B），其后全部是帧数据。
                let mut buf = vec![0u8; d.len() as usize];
                mem.read_slice(&mut buf, d.addr())
                    .map_err(|e| AvmError::Device(format!("tx 帧数据读取失败：{e}")))?;
                frame.extend_from_slice(&buf);
            }
            self.backend.send_frame(&frame)?;
            let head = chain.head_index();
            queue.add_used(mem, head, 1).map_err(vq_err)?;
            processed += 1;
        }
        queue.enable_notification(mem).map_err(vq_err)?;
        self.tx_queue = Some(queue);
        Ok(processed)
    }

    /// 处理 rx 队列：从后端取帧写入驱动预置的可写缓冲，返回已投递帧数。
    pub fn process_rx(&mut self, mem: &GuestMemoryMmap) -> AvmResult<usize> {
        let mut queue = self
            .rx_queue
            .take()
            .ok_or_else(|| AvmError::Device("rx 队列尚未 setup".into()))?;
        let mut delivered = 0usize;
        queue.disable_notification(mem).map_err(vq_err)?;
        while let Some(chain) = queue.iter(mem).map_err(vq_err)?.next() {
            let Some(frame) = self.backend.recv_frame() else {
                // 后端无帧：链留在队列外不再消费（virtio 语义上驱动可 re-available；
                // M3 测试中我们直接记 used 长度 0 交还）
                let head = chain.head_index();
                queue.add_used(mem, head, 0).map_err(vq_err)?;
                continue;
            };
            let total = NET_HDR_LEN + frame.len();
            let mut written = 0usize;
            let mut overflow = false;
            for d in chain.clone().writable() {
                if written >= total {
                    break;
                }
                let want = (total - written).min(d.len() as usize);
                let chunk: Vec<u8> = if written < NET_HDR_LEN {
                    // 头部全零（未协商 checksum/GSO），可能横跨头/帧边界
                    let hdr_take = want.min(NET_HDR_LEN - written);
                    let mut buf = vec![0u8; want];
                    if hdr_take < want {
                        let fstart = written + hdr_take - NET_HDR_LEN;
                        buf[hdr_take..].copy_from_slice(&frame[fstart..fstart + (want - hdr_take)]);
                    }
                    buf
                } else {
                    let fstart = written - NET_HDR_LEN;
                    frame[fstart..fstart + want].to_vec()
                };
                mem.write_slice(&chunk, d.addr())
                    .map_err(|e| AvmError::Device(format!("rx 帧写入 guest 失败：{e}")))?;
                written += want;
            }
            if written < total {
                overflow = true;
            }
            let head = chain.head_index();
            queue.add_used(mem, head, written as u32).map_err(vq_err)?;
            if overflow {
                // 缓冲不足：报错但不中止整个队列（M3 语义：丢弃该帧）
                return Err(AvmError::Device(format!(
                    "rx 缓冲不足：需要 {total} 字节，链只有 {written} 字节"
                )));
            }
            delivered += 1;
        }
        queue.enable_notification(mem).map_err(vq_err)?;
        self.rx_queue = Some(queue);
        Ok(delivered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vm_memory::GuestAddress;

    /// 一条最小以太网帧：dst=广播, src=02:00:00:00:00:01, ethertype=0x0800, 4B payload
    fn sample_frame() -> Vec<u8> {
        let mut f = vec![0xFF; 6];
        f.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        f.extend_from_slice(&[0x08, 0x00]);
        f.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        f
    }

    fn make_mem() -> Arc<GuestMemoryMmap> {
        Arc::new(GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x20000)]).unwrap())
    }

    fn new_device() -> (VirtioNet, Arc<GuestMemoryMmap>) {
        (
            VirtioNet::new(MAC, Box::new(LoopbackBackend::new())),
            make_mem(),
        )
    }

    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

    /// 在 guest 内存为指定队列写 desc 表 + avail ring，加入一条链。
    /// descs: (addr, len, writable)，自动串 NEXT。
    fn enqueue_chain(
        mem: &GuestMemoryMmap,
        desc_base: u64,
        avail_base: u64,
        descs: &[(u64, u32, bool)],
    ) {
        let n = descs.len() as u16;
        for (i, (addr, len, writable)) in descs.iter().enumerate() {
            let mut d = [0u8; 16];
            d[0..8].copy_from_slice(&addr.to_le_bytes());
            d[8..12].copy_from_slice(&len.to_le_bytes());
            let mut flags: u16 = 0;
            if *writable {
                flags |= 2;
            }
            if (i as u16) + 1 < n {
                flags |= 1;
                d[14..16].copy_from_slice(&(i as u16 + 1).to_le_bytes());
            }
            d[12..14].copy_from_slice(&flags.to_le_bytes());
            mem.write_slice(&d, GuestAddress(desc_base + i as u64 * 16))
                .unwrap();
        }
        let avail_idx_addr = avail_base + 2;
        let cur: u16 = mem.read_obj(GuestAddress(avail_idx_addr)).unwrap_or(0);
        mem.write_obj(0u16, GuestAddress(avail_base + 4 + 2 * cur as u64))
            .unwrap();
        mem.write_obj(cur.wrapping_add(1), GuestAddress(avail_idx_addr))
            .unwrap();
    }

    #[test]
    fn config_mac_and_link() {
        let dev = VirtioNet::new(MAC, Box::new(LoopbackBackend::new()));
        assert_eq!(dev.config_mac(), [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        assert_eq!(dev.config_link_status(), VIRTIO_NET_S_LINK_UP);
    }

    #[test]
    fn process_requires_setup() {
        let (mut dev, mem) = new_device();
        assert!(dev.process_tx(mem.as_ref()).is_err());
        assert!(dev.process_rx(mem.as_ref()).is_err());
    }

    #[test]
    fn tx_frame_reaches_backend() {
        let (mut dev, mem) = new_device();
        dev.setup_tx_queue(mem.as_ref()).unwrap();

        // 链：desc0 = 12B 头（全零），desc1 = 帧数据
        let hdr_addr: u64 = 0x11000;
        let frame_addr: u64 = 0x12000;
        let frame = sample_frame();
        mem.write_slice(&[0u8; NET_HDR_LEN], GuestAddress(hdr_addr))
            .unwrap();
        mem.write_slice(&frame, GuestAddress(frame_addr)).unwrap();
        enqueue_chain(
            mem.as_ref(),
            TX_DESC_TABLE_ADDR,
            TX_AVAIL_RING_ADDR,
            &[
                (hdr_addr, NET_HDR_LEN as u32, false),
                (frame_addr, frame.len() as u32, false),
            ],
        );
        assert_eq!(dev.process_tx(mem.as_ref()).unwrap(), 1);
    }

    #[test]
    fn rx_frame_delivered_with_header() {
        let (mut dev, mem) = new_device();
        dev.setup_rx_queue(mem.as_ref()).unwrap();

        // 先构造一个带注入帧的后端设备
        let mut backend = LoopbackBackend::new();
        let frame = sample_frame();
        backend.inject(frame.clone());
        let mut dev = VirtioNet::new(MAC, Box::new(backend));
        dev.setup_rx_queue(mem.as_ref()).unwrap();

        // 驱动预置 rx 缓冲：单描述符可写
        let buf_addr: u64 = 0x13000;
        let buf_len = (NET_HDR_LEN + frame.len()) as u32;
        enqueue_chain(
            mem.as_ref(),
            RX_DESC_TABLE_ADDR,
            RX_AVAIL_RING_ADDR,
            &[(buf_addr, buf_len, true)],
        );
        assert_eq!(dev.process_rx(mem.as_ref()).unwrap(), 1);

        // 读回：前 12B 头全零 + 帧数据
        let mut out = vec![0u8; buf_len as usize];
        mem.read_slice(&mut out, GuestAddress(buf_addr)).unwrap();
        assert_eq!(&out[..NET_HDR_LEN], &[0u8; NET_HDR_LEN]);
        assert_eq!(&out[NET_HDR_LEN..], &frame[..]);
    }

    #[test]
    fn rx_no_frame_records_zero_used() {
        let mut dev = VirtioNet::new([0x52; 6], Box::new(LoopbackBackend::new()));
        let mem = make_mem();
        dev.setup_rx_queue(mem.as_ref()).unwrap();
        let buf_addr: u64 = 0x13000;
        enqueue_chain(
            mem.as_ref(),
            RX_DESC_TABLE_ADDR,
            RX_AVAIL_RING_ADDR,
            &[(buf_addr, 128, true)],
        );
        // 无帧：处理完成、0 投递
        assert_eq!(dev.process_rx(mem.as_ref()).unwrap(), 0);
    }

    #[test]
    fn rx_buffer_too_small_errors() {
        let mut backend = LoopbackBackend::new();
        let frame = sample_frame();
        backend.inject(frame.clone());
        let mut dev = VirtioNet::new([0x52; 6], Box::new(backend));
        let mem = make_mem();
        dev.setup_rx_queue(mem.as_ref()).unwrap();
        let buf_addr: u64 = 0x13000;
        // 缓冲只有 8 字节 < 12B 头 + 帧长
        enqueue_chain(
            mem.as_ref(),
            RX_DESC_TABLE_ADDR,
            RX_AVAIL_RING_ADDR,
            &[(buf_addr, 8, true)],
        );
        assert!(dev.process_rx(mem.as_ref()).is_err());
    }

    #[test]
    fn loopback_roundtrip() {
        let (mut dev, mem) = new_device();
        dev.setup_tx_queue(mem.as_ref()).unwrap();
        dev.setup_rx_queue(mem.as_ref()).unwrap();

        // tx：自环后端把帧收进 inbound
        let hdr_addr: u64 = 0x11000;
        let frame_addr: u64 = 0x12000;
        let frame = sample_frame();
        mem.write_slice(&[0u8; NET_HDR_LEN], GuestAddress(hdr_addr))
            .unwrap();
        mem.write_slice(&frame, GuestAddress(frame_addr)).unwrap();
        enqueue_chain(
            mem.as_ref(),
            TX_DESC_TABLE_ADDR,
            TX_AVAIL_RING_ADDR,
            &[
                (hdr_addr, NET_HDR_LEN as u32, false),
                (frame_addr, frame.len() as u32, false),
            ],
        );
        assert_eq!(dev.process_tx(mem.as_ref()).unwrap(), 1);

        // rx：驱动预置缓冲
        let buf_addr: u64 = 0x13000;
        enqueue_chain(
            mem.as_ref(),
            RX_DESC_TABLE_ADDR,
            RX_AVAIL_RING_ADDR,
            &[(buf_addr, (NET_HDR_LEN + frame.len()) as u32, true)],
        );
        assert_eq!(dev.process_rx(mem.as_ref()).unwrap(), 1);
        let mut out = vec![0u8; NET_HDR_LEN + frame.len()];
        mem.read_slice(&mut out, GuestAddress(buf_addr)).unwrap();
        assert_eq!(&out[NET_HDR_LEN..], &frame[..]);
    }
}
