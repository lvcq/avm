//! virtio-blk 设备模型（M2）。
//!
//! - 队列：virtio-queue `Queue`（split virtqueue）
//! - 请求：VIRTIO_BLK 协议（header 16B = type/reserved/sector；status 1B）
//! - 后端：内存块设备（M2 测试用；真机镜像后端在 M5 接入）

use avm_core::{AvmError, AvmResult};
use virtio_queue::Queue;
use virtio_queue::QueueT;
use virtio_queue::{Error as VqError, QueueOwnedT};
use vm_memory::{Address, Bytes, GuestAddress, GuestMemoryMmap};

/// 请求类型：读（guest <- 设备）。
pub const VIRTIO_BLK_T_IN: u32 = 0;
/// 请求类型：写（guest -> 设备）。
pub const VIRTIO_BLK_T_OUT: u32 = 1;
/// 状态：成功。
pub const VIRTIO_BLK_S_OK: u8 = 0;
/// 状态：IO 错误。
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
/// 状态：不支持的请求。
pub const VIRTIO_BLK_S_UNSUP: u8 = 2;

fn vq_err(e: VqError) -> AvmError {
    AvmError::Device(format!("virtqueue 错误：{e}"))
}

/// u64 拆为 (low32, high32)（virtio-pci modern 双 32 位寄存器语义）。
pub fn split64(v: u64) -> (u32, u32) {
    ((v & 0xFFFF_FFFF) as u32, (v >> 32) as u32)
}

/// 扇区大小（字节）。
pub const SECTOR_SIZE: u64 = 512;
/// 单队列深度。
pub const QUEUE_SIZE: u16 = 64;
/// 队列描述区（desc_table/avail_ring/used_ring 在 guest 内存中的布局）。
pub const DESC_TABLE_ADDR: u64 = 0x2000;
pub const AVAIL_RING_ADDR: u64 = 0x3000;
pub const USED_RING_ADDR: u64 = 0x4000;

/// 请求头（16 字节，LE）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlkRequestHeader {
    pub req_type: u32,
    pub reserved: u32,
    pub sector: u64,
}

impl BlkRequestHeader {
    fn from_bytes(b: &[u8]) -> Result<Self, AvmError> {
        if b.len() < 16 {
            return Err(AvmError::Device("请求头不足 16 字节".into()));
        }
        Ok(Self {
            req_type: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            reserved: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            sector: u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
        })
    }
}

/// virtio-blk 设备。
pub struct VirtioBlk {
    /// 后端磁盘（M2 内存后端）。
    disk: Vec<u8>,
    queue: Option<Queue>,
}

impl VirtioBlk {
    /// 创建设备，磁盘容量取整到扇区。
    pub fn new(capacity_mib: u64) -> Self {
        let bytes = (capacity_mib * 1024 * 1024) as usize;
        Self {
            disk: vec![0; bytes],
            queue: None,
        }
    }

    /// 配置空间容量字段（扇区数，LE u64）。
    pub fn config_capacity(&self) -> [u8; 8] {
        (self.disk.len() as u64 / SECTOR_SIZE).to_le_bytes()
    }

    pub fn capacity_sectors(&self) -> u64 {
        self.disk.len() as u64 / SECTOR_SIZE
    }

    /// 按 virtio-mmio/PCI 约定地址布置队列（驱动写好 desc/avail 后调用）。
    pub fn setup_queue(&mut self, mem: &GuestMemoryMmap) -> AvmResult<()> {
        let mut queue = Queue::new(QUEUE_SIZE)
            .map_err(|e| AvmError::Device(format!("virtqueue 创建失败：{e}")))?;
        queue.set_size(QUEUE_SIZE);
        let (dl, dh) = split64(DESC_TABLE_ADDR);
        queue.set_desc_table_address(Some(dl), Some(dh));
        let (al, ah) = split64(AVAIL_RING_ADDR);
        queue.set_avail_ring_address(Some(al), Some(ah));
        let (ul, uh) = split64(USED_RING_ADDR);
        queue.set_used_ring_address(Some(ul), Some(uh));
        queue.set_ready(true);
        if !queue.is_valid(mem) {
            return Err(AvmError::Device(
                "virtqueue 状态无效（地址/大小未通过校验）".into(),
            ));
        }
        self.queue = Some(queue);
        Ok(())
    }

    /// 处理 avail 队列中全部请求，返回已处理数量。
    pub fn process_queue(&mut self, mem: &GuestMemoryMmap) -> AvmResult<usize> {
        // 临时取出队列避免与 self.disk 的可变借用冲突
        let mut queue = self
            .queue
            .take()
            .ok_or_else(|| AvmError::Device("virtqueue 尚未 setup".into()))?;

        let mut processed = 0usize;
        let mut aborted: Option<AvmError> = None;
        queue.disable_notification(mem).map_err(vq_err)?;
        while let Some(chain) = queue.iter(mem).map_err(vq_err)?.next() {
            let status = self.execute_request(mem, &chain);
            let head = chain.head_index();
            queue.add_used(mem, head, 1).map_err(vq_err)?;
            if let Err(e) = status {
                aborted = Some(e);
                break;
            }
            processed += 1;
        }
        let notified = queue.enable_notification(mem).map_err(vq_err)?;
        let _ = notified;
        self.queue = Some(queue);
        match aborted {
            Some(e) => Err(e),
            None => Ok(processed),
        }
    }

    /// 执行单个请求并写回状态字节。
    fn execute_request(
        &mut self,
        mem: &GuestMemoryMmap,
        chain: &virtio_queue::DescriptorChain<&GuestMemoryMmap>,
    ) -> AvmResult<()> {
        // 收集 readable（header [+ 写请求数据]）与 writable（[读请求数据] + status）
        let mut readable: Vec<(u64, u32)> = Vec::new();
        for d in chain.clone().readable() {
            readable.push((d.addr().raw_value(), d.len()));
        }
        let mut writable: Vec<(u64, u32)> = Vec::new();
        for d in chain.clone().writable() {
            writable.push((d.addr().raw_value(), d.len()));
        }
        if readable.is_empty() || writable.is_empty() {
            return Err(AvmError::Device("virtio-blk 描述链形状不完整".into()));
        }

        // 读取请求头
        let mut hdr = [0u8; 16];
        let (haddr, hlen) = readable[0];
        if hlen < 16 {
            return Err(AvmError::Device("请求头描述符 < 16 字节".into()));
        }
        mem.read_slice(&mut hdr, GuestAddress(haddr))
            .map_err(|e| AvmError::Device(format!("请求头读取失败：{e}")))?;
        let req = BlkRequestHeader::from_bytes(&hdr)?;

        // status = 最后一个 writable 描述符的第一个字节
        let (status_addr, _) = *writable.last().unwrap();

        // 数据描述符：读操作在 writable[0..n-1]，写操作在 readable[1..]
        let result = match req.req_type {
            VIRTIO_BLK_T_IN => self.do_read(mem, req.sector, &writable[..writable.len() - 1]),
            VIRTIO_BLK_T_OUT => self.do_write(mem, req.sector, &readable[1..]),
            _ => {
                mem.write_obj(VIRTIO_BLK_S_UNSUP, GuestAddress(status_addr))
                    .map_err(|e| AvmError::Device(format!("状态写入失败：{e}")))?;
                return Ok(());
            }
        };

        // 数据面 IO 错误以 IOERR 状态回告驱动，不中止整个队列
        if result.is_err() {
            mem.write_obj(VIRTIO_BLK_S_IOERR, GuestAddress(status_addr))
                .map_err(|e| AvmError::Device(format!("状态写入失败：{e}")))?;
            return Ok(());
        }
        let status = match &result {
            Ok(()) => VIRTIO_BLK_S_OK,
            Err(_) => VIRTIO_BLK_S_IOERR,
        };
        mem.write_obj(status, GuestAddress(status_addr))
            .map_err(|e| AvmError::Device(format!("状态写入失败：{e}")))?;
        result
    }

    fn do_read(&self, mem: &GuestMemoryMmap, sector: u64, descs: &[(u64, u32)]) -> AvmResult<()> {
        let mut offset = sector * SECTOR_SIZE;
        for (addr, len) in descs {
            let end = offset + *len as u64;
            if end > self.disk.len() as u64 {
                return Err(AvmError::Device(format!(
                    "读越界：offset=0x{offset:x} len={len}"
                )));
            }
            mem.write_slice(
                &self.disk[offset as usize..end as usize],
                GuestAddress(*addr),
            )
            .map_err(|e| AvmError::Device(format!("读数据写入 guest 失败：{e}")))?;
            offset = end;
        }
        Ok(())
    }

    fn do_write(
        &mut self,
        mem: &GuestMemoryMmap,
        sector: u64,
        descs: &[(u64, u32)],
    ) -> AvmResult<()> {
        let mut offset = sector * SECTOR_SIZE;
        for (addr, len) in descs {
            let end = offset + *len as u64;
            if end > self.disk.len() as u64 {
                return Err(AvmError::Device(format!(
                    "写越界：offset=0x{offset:x} len={len}"
                )));
            }
            mem.read_slice(
                &mut self.disk[offset as usize..end as usize],
                GuestAddress(*addr),
            )
            .map_err(|e| AvmError::Device(format!("写数据读取 guest 失败：{e}")))?;
            offset = end;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn new_device_with_queue() -> (VirtioBlk, Arc<GuestMemoryMmap>) {
        let mem = Arc::new(GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap());
        let mut dev = VirtioBlk::new(1); // 1 MiB
        dev.setup_queue(mem.as_ref()).unwrap();
        (dev, mem)
    }

    /// 在 guest 内存构造描述符表 + avail ring，加入一个请求链。
    /// 返回链头索引与各描述符地址。
    fn enqueue_request(
        mem: &GuestMemoryMmap,
        head_idx: u16,
        req_type: u32,
        sector: u64,
        data_addr: u64,
        data_len: u32,
        data_writable: bool,
        status_addr: u64,
    ) {
        // desc 布局：addr u64 | len u32 | flags u16 | next u16 = 16 字节
        let write_desc = |idx: u16, addr: u64, len: u32, flags: u16, next: u16| {
            let mut d = [0u8; 16];
            d[0..8].copy_from_slice(&addr.to_le_bytes());
            d[8..12].copy_from_slice(&len.to_le_bytes());
            d[12..14].copy_from_slice(&flags.to_le_bytes());
            d[14..16].copy_from_slice(&next.to_le_bytes());
            mem.write_slice(&d, GuestAddress(DESC_TABLE_ADDR + idx as u64 * 16))
                .unwrap();
        };

        // desc0: header（readable，0x5000），desc1: data，desc2: status（0x6000）
        let mut hdr = [0u8; 16];
        hdr[0..4].copy_from_slice(&req_type.to_le_bytes());
        hdr[8..16].copy_from_slice(&sector.to_le_bytes());
        let hdr_addr: u64 = 0x5000;
        mem.write_slice(&hdr, GuestAddress(hdr_addr)).unwrap();
        let f_write: u16 = 2; // VRING_DESC_F_WRITE
        let f_next: u16 = 1; // VRING_DESC_F_NEXT
        let data_flags = if data_writable {
            f_write | f_next
        } else {
            f_next
        };
        write_desc(0, hdr_addr, 16, f_next, 1);
        write_desc(1, data_addr, data_len, data_flags, 2);
        write_desc(2, status_addr, 1, f_write, 0);

        // avail ring：flag(0x3000)=0, idx(0x3002), elem(0x3004+2*idx)
        let avail_idx_addr = AVAIL_RING_ADDR + 2;
        let cur: u16 = mem.read_obj(GuestAddress(avail_idx_addr)).unwrap_or(0);
        mem.write_obj(head_idx, GuestAddress(AVAIL_RING_ADDR + 4 + 2 * cur as u64))
            .unwrap();
        mem.write_obj(cur.wrapping_add(1), GuestAddress(avail_idx_addr))
            .unwrap();
    }

    #[test]
    fn capacity_config_matches_disk() {
        let dev = VirtioBlk::new(2);
        assert_eq!(dev.capacity_sectors(), 4096);
        let cap = dev.config_capacity();
        assert_eq!(u64::from_le_bytes(cap), 4096);
    }

    #[test]
    fn setup_queue_twice_ok() {
        let (mut dev, mem) = new_device_with_queue();
        dev.setup_queue(mem.as_ref()).unwrap(); // 重复 setup 不 panic
    }

    #[test]
    fn process_queue_requires_setup() {
        let mut dev = VirtioBlk::new(1);
        let mem = Arc::new(GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap());
        assert!(dev.process_queue(mem.as_ref()).is_err());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let (mut dev, mem) = new_device_with_queue();

        // 写请求：sector 0，写 512 字节标记数据
        let data_addr: u64 = 0x7000;
        let payload: Vec<u8> = (0..512u32).map(|i| (i % 251) as u8).collect();
        mem.write_slice(&payload, GuestAddress(data_addr)).unwrap();
        let status_addr: u64 = 0x6000;
        enqueue_request(
            mem.as_ref(),
            0,
            VIRTIO_BLK_T_OUT,
            0,
            data_addr,
            512,
            false,
            status_addr,
        );
        assert_eq!(dev.process_queue(mem.as_ref()).unwrap(), 1);
        assert_eq!(
            mem.read_obj::<u8>(GuestAddress(status_addr)).unwrap(),
            VIRTIO_BLK_S_OK
        );
        assert_eq!(&dev.disk[..512], &payload[..]);

        // 读请求：sector 0 → 数据应回到 guest（用新 data 区域）
        let read_addr: u64 = 0x8000;
        mem.write_obj::<u8>(0xFF, GuestAddress(status_addr))
            .unwrap();
        enqueue_request(
            mem.as_ref(),
            0,
            VIRTIO_BLK_T_IN,
            0,
            read_addr,
            512,
            true,
            status_addr,
        );
        assert_eq!(dev.process_queue(mem.as_ref()).unwrap(), 1);
        assert_eq!(
            mem.read_obj::<u8>(GuestAddress(status_addr)).unwrap(),
            VIRTIO_BLK_S_OK
        );
        let mut out = vec![0u8; 512];
        mem.read_slice(&mut out, GuestAddress(read_addr)).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn out_of_range_read_reports_ioerr() {
        let (mut dev, mem) = new_device_with_queue();
        let status_addr: u64 = 0x6000;
        // 1MiB = 2048 扇区；读 sector 4000 → 只有 48 扇区可读，512 字节缓冲越界
        enqueue_request(
            mem.as_ref(),
            0,
            VIRTIO_BLK_T_IN,
            2048,
            0x7000,
            512,
            true,
            status_addr,
        );
        assert_eq!(dev.process_queue(mem.as_ref()).unwrap(), 1);
        assert_eq!(
            mem.read_obj::<u8>(GuestAddress(status_addr)).unwrap(),
            VIRTIO_BLK_S_IOERR
        );
    }

    #[test]
    fn unsupported_request_type_reports_unsup() {
        let (mut dev, mem) = new_device_with_queue();
        let status_addr: u64 = 0x6000;
        enqueue_request(mem.as_ref(), 0, 0x7FFF, 0, 0x7000, 512, false, status_addr);
        assert_eq!(dev.process_queue(mem.as_ref()).unwrap(), 1);
        assert_eq!(
            mem.read_obj::<u8>(GuestAddress(status_addr)).unwrap(),
            VIRTIO_BLK_S_UNSUP
        );
    }

    #[test]
    fn header_parse() {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&VIRTIO_BLK_T_IN.to_le_bytes());
        b[8..16].copy_from_slice(&0x1234u64.to_le_bytes());
        let h = BlkRequestHeader::from_bytes(&b).unwrap();
        assert_eq!(h.req_type, VIRTIO_BLK_T_IN);
        assert_eq!(h.sector, 0x1234);
        assert!(BlkRequestHeader::from_bytes(&[0u8; 8]).is_err());
    }
}
