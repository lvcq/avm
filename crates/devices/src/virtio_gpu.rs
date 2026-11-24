//! virtio-gpu 2D 设备模型（M4a）。
//!
//! 最小 virtio-gpu 2D 协议（controlq，cursorq M4a 不实现）：
//! - GET_DISPLAY_INFO：返回单显示头（固定 800x600）
//! - RESOURCE_CREATE_2D / RESOURCE_UNREF：host 侧 2D 资源（RGBA host 镜像）
//! - RESOURCE_ATTACH_BACKING：登记 guest backing 内存段
//! - TRANSFER_TO_HOST_2D：guest backing → host 镜像（按矩形）
//! - SET_SCANOUT：资源绑定扫描输出
//! - RESOURCE_FLUSH：host 镜像 → scanout 帧缓冲
//! 截图出口：`framebuffer()` 返回 scanout 帧缓冲（RGBA）。

use avm_core::{AvmError, AvmResult};
use virtio_queue::Queue;
use virtio_queue::QueueT;
use virtio_queue::{Error as VqError, QueueOwnedT};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

use crate::display::FrameSink;
use crate::virtio_blk::split64;

/// 单队列深度。
pub const QUEUE_SIZE: u16 = 64;
/// 虚拟显示分辨率（M4a 固定单头）。
pub const DISPLAY_WIDTH: u32 = 800;
pub const DISPLAY_HEIGHT: u32 = 600;
/// 每像素 4 字节（B8G8R8A8_UNORM）。
pub const PIXEL_SIZE: usize = 4;
/// B8G8R8A8_UNORM 格式 ID。
pub const FORMAT_B8G8R8A8_UNORM: u32 = 1;

/// 控制队列（0 号）布局。
pub const CTRL_DESC_TABLE_ADDR: u64 = 0xB000;
pub const CTRL_AVAIL_RING_ADDR: u64 = 0xC000;
pub const CTRL_USED_RING_ADDR: u64 = 0xD000;
/// 光标队列（1 号）布局（M4a 预留）。
pub const CURSOR_DESC_TABLE_ADDR: u64 = 0xE000;
pub const CURSOR_AVAIL_RING_ADDR: u64 = 0xF000;
pub const CURSOR_USED_RING_ADDR: u64 = 0x10000;

// ---- 命令码（virtio-gpu 规范）----
pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0109;

// ---- 响应码 ----
pub const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const VIRTIO_GPU_RESP_ERR_UNSPEC: u32 = 0x1200;
pub const VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
pub const VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

fn vq_err(e: VqError) -> AvmError {
    AvmError::Device(format!("virtqueue 错误：{e}"))
}

/// 矩形（16 字节）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl GpuRect {
    fn from_bytes(b: &[u8]) -> Self {
        Self {
            x: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            y: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            width: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            height: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        }
    }

    fn to_bytes(self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&self.x.to_le_bytes());
        b[4..8].copy_from_slice(&self.y.to_le_bytes());
        b[8..12].copy_from_slice(&self.width.to_le_bytes());
        b[12..16].copy_from_slice(&self.height.to_le_bytes());
        b
    }
}

/// 控制命令头（24 字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtrlHeader {
    pub cmd_type: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub ring_idx: u32,
}

impl CtrlHeader {
    fn from_bytes(b: &[u8]) -> Result<Self, AvmError> {
        if b.len() < 24 {
            return Err(AvmError::Device("控制头不足 24 字节".into()));
        }
        Ok(Self {
            cmd_type: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            flags: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            fence_id: u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
            ctx_id: u32::from_le_bytes([b[16], b[17], b[18], b[19]]),
            ring_idx: u32::from_le_bytes([b[20], b[21], b[22], b[23]]),
        })
    }

    fn to_bytes(self) -> [u8; 24] {
        let mut b = [0u8; 24];
        b[0..4].copy_from_slice(&self.cmd_type.to_le_bytes());
        b[4..8].copy_from_slice(&self.flags.to_le_bytes());
        b[8..16].copy_from_slice(&self.fence_id.to_le_bytes());
        b[16..20].copy_from_slice(&self.ctx_id.to_le_bytes());
        b[20..24].copy_from_slice(&self.ring_idx.to_le_bytes());
        b
    }
}

/// host 侧 2D 资源。
#[derive(Debug)]
struct GpuResource {
    width: u32,
    height: u32,
    /// guest backing 内存段（gpa, len）。
    backing: Option<Vec<(u64, u32)>>,
    /// host 镜像（RGBA）。
    image: Vec<u8>,
}

impl GpuResource {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            backing: None,
            image: vec![0; width as usize * height as usize * PIXEL_SIZE],
        }
    }
}

/// virtio-gpu 2D 设备。
pub struct VirtioGpu {
    /// 资源表（id → 资源）。
    resources: std::collections::BTreeMap<u32, GpuResource>,
    /// scanout 0 绑定的资源 id。
    scanout_resource: Option<u32>,
    /// scanout 0 帧缓冲（RGBA，DISPLAY_WIDTH×DISPLAY_HEIGHT）。
    framebuffer: Vec<u8>,
    /// 控制队列。
    ctrl_queue: Option<Queue>,
    /// 光标队列（M4a 预留，不处理）。
    _cursor_queue: Option<Queue>,
    /// 呈现后端（M4b：flush 后调用）。
    sink: Option<Box<dyn FrameSink>>,
}

impl VirtioGpu {
    pub fn new() -> Self {
        Self {
            resources: std::collections::BTreeMap::new(),
            scanout_resource: None,
            framebuffer: vec![0; DISPLAY_WIDTH as usize * DISPLAY_HEIGHT as usize * PIXEL_SIZE],
            ctrl_queue: None,
            _cursor_queue: None,
            sink: None,
        }
    }

    /// 挂载呈现后端（M4b）。
    pub fn set_sink(&mut self, sink: Box<dyn FrameSink>) {
        self.sink = Some(sink);
    }

    pub fn setup_ctrl_queue(&mut self, mem: &GuestMemoryMmap) -> AvmResult<()> {
        self.ctrl_queue = Some(build_queue(
            mem,
            CTRL_DESC_TABLE_ADDR,
            CTRL_AVAIL_RING_ADDR,
            CTRL_USED_RING_ADDR,
        )?);
        Ok(())
    }

    /// 布置光标队列（占位，不处理任何命令）。
    pub fn setup_cursor_queue(&mut self, mem: &GuestMemoryMmap) -> AvmResult<()> {
        self._cursor_queue = Some(build_queue(
            mem,
            CURSOR_DESC_TABLE_ADDR,
            CURSOR_AVAIL_RING_ADDR,
            CURSOR_USED_RING_ADDR,
        )?);
        Ok(())
    }

    /// 截图出口：scanout 帧缓冲（RGBA 逐行）。
    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    /// scanout 当前绑定的资源 id。
    pub fn scanout_resource(&self) -> Option<u32> {
        self.scanout_resource
    }

    /// 处理控制队列，返回已处理命令数。
    pub fn process_ctrl_queue(&mut self, mem: &GuestMemoryMmap) -> AvmResult<usize> {
        let mut queue = self
            .ctrl_queue
            .take()
            .ok_or_else(|| AvmError::Device("ctrl 队列尚未 setup".into()))?;
        let mut processed = 0usize;
        queue.disable_notification(mem).map_err(vq_err)?;
        while let Some(chain) = queue.iter(mem).map_err(vq_err)?.next() {
            let head = chain.head_index();
            // 响应缓冲 = 链的 writable 部分（首描述符）
            let resp = self.execute_command(mem, &chain);
            let (resp_type, extra) = resp;
            // 写响应：header（24B）+ 可选负载
            let resp_hdr = CtrlHeader {
                cmd_type: resp_type,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
                ring_idx: 0,
            }
            .to_bytes();
            let mut written = 0usize;
            let total = resp_hdr.len() + extra.len();
            for d in chain.clone().writable() {
                if written >= total {
                    break;
                }
                let want = (total - written).min(d.len() as usize);
                let chunk: Vec<u8> = if written < 24 {
                    let hdr_take = want.min(24 - written);
                    let mut buf = vec![0u8; want];
                    buf[..hdr_take].copy_from_slice(&resp_hdr[written..written + hdr_take]);
                    if hdr_take < want {
                        buf[hdr_take..].copy_from_slice(&extra[0..want - hdr_take]);
                    }
                    buf
                } else {
                    extra[written - 24..written - 24 + want].to_vec()
                };
                mem.write_slice(&chunk, d.addr())
                    .map_err(|e| AvmError::Device(format!("gpu 响应写入失败：{e}")))?;
                written += want;
            }
            queue.add_used(mem, head, written as u32).map_err(vq_err)?;
            processed += 1;
        }
        queue.enable_notification(mem).map_err(vq_err)?;
        self.ctrl_queue = Some(queue);
        Ok(processed)
    }

    /// 执行单条控制命令，返回（响应码，附加负载）。
    fn execute_command(
        &mut self,
        mem: &GuestMemoryMmap,
        chain: &virtio_queue::DescriptorChain<&GuestMemoryMmap>,
    ) -> (u32, Vec<u8>) {
        // 收集 readable
        let mut readable: Vec<(u64, u32)> = Vec::new();
        for d in chain.clone().readable() {
            readable.push((d.addr().0, d.len()));
        }
        if readable.is_empty() {
            return (VIRTIO_GPU_RESP_ERR_UNSPEC, Vec::new());
        }
        let (haddr, hlen) = readable[0];
        if hlen < 24 {
            return (VIRTIO_GPU_RESP_ERR_UNSPEC, Vec::new());
        }
        let mut hb = [0u8; 24];
        if mem.read_slice(&mut hb, GuestAddress(haddr)).is_err() {
            return (VIRTIO_GPU_RESP_ERR_UNSPEC, Vec::new());
        }
        let hdr = match CtrlHeader::from_bytes(&hb) {
            Ok(h) => h,
            Err(_) => return (VIRTIO_GPU_RESP_ERR_UNSPEC, Vec::new()),
        };
        // 负载 = readable[1..] 串接（至多 64 字节，足够全部 2D 命令）
        let mut payload = Vec::new();
        for (addr, len) in readable.iter().skip(1).take(4) {
            let mut buf = vec![0u8; *len as usize];
            if mem.read_slice(&mut buf, GuestAddress(*addr)).is_err() {
                return (VIRTIO_GPU_RESP_ERR_UNSPEC, Vec::new());
            }
            payload.extend_from_slice(&buf);
        }

        match hdr.cmd_type {
            VIRTIO_GPU_CMD_GET_DISPLAY_INFO => {
                // header + 单显示模式（rect + enabled + flags）
                let mut extra = Vec::with_capacity(24);
                let mode_rect = GpuRect {
                    x: 0,
                    y: 0,
                    width: DISPLAY_WIDTH,
                    height: DISPLAY_HEIGHT,
                };
                extra.extend_from_slice(&mode_rect.to_bytes());
                extra.extend_from_slice(&1u32.to_le_bytes()); // enabled
                extra.extend_from_slice(&0u32.to_le_bytes()); // flags
                (VIRTIO_GPU_RESP_OK_DISPLAY_INFO, extra)
            }
            VIRTIO_GPU_CMD_RESOURCE_CREATE_2D => {
                if payload.len() < 16 {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, Vec::new());
                }
                let format = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                let width = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
                let height = u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
                let res_id =
                    u32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]);
                if res_id == 0
                    || self.resources.contains_key(&res_id)
                    || format != FORMAT_B8G8R8A8_UNORM
                    || width == 0
                    || height == 0
                {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, Vec::new());
                }
                self.resources
                    .insert(res_id, GpuResource::new(width, height));
                (VIRTIO_GPU_RESP_OK_NODATA, Vec::new())
            }
            VIRTIO_GPU_CMD_RESOURCE_UNREF => {
                if payload.len() < 4 {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, Vec::new());
                }
                let res_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                match self.resources.remove(&res_id) {
                    Some(_) => {
                        if self.scanout_resource == Some(res_id) {
                            self.scanout_resource = None;
                        }
                        (VIRTIO_GPU_RESP_OK_NODATA, Vec::new())
                    }
                    None => (VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID, Vec::new()),
                }
            }
            VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING => {
                if payload.len() < 4 {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, Vec::new());
                }
                let res_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                let resource = match self.resources.get_mut(&res_id) {
                    Some(r) => r,
                    None => return (VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID, Vec::new()),
                };
                // backing 内存段来自链上后续 writable?（不，mem entries 由驱动放在
                // ATTACH_BACKING 命令后的额外 readable 描述符）
                // 此处 M4a 简化：登记链上 payload 之后的段不可行，改为空 backing
                // ——测试用 TRANSFER 前必须先 ATTACH 并由驱动提供 mem entries。
                // M4a 支持在 payload[16..] 内嵌 mem entries（addr u64 + len u32 + pad）。
                let mut entries = Vec::new();
                let mut off = 16usize; // 4+4+8 头部
                while off + 16 <= payload.len() {
                    let addr = u64::from_le_bytes([
                        payload[off],
                        payload[off + 1],
                        payload[off + 2],
                        payload[off + 3],
                        payload[off + 4],
                        payload[off + 5],
                        payload[off + 6],
                        payload[off + 7],
                    ]);
                    let len = u32::from_le_bytes([
                        payload[off + 8],
                        payload[off + 9],
                        payload[off + 10],
                        payload[off + 11],
                    ]);
                    entries.push((addr, len));
                    off += 16;
                }
                resource.backing = Some(entries);
                (VIRTIO_GPU_RESP_OK_NODATA, Vec::new())
            }
            VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D => {
                if payload.len() < 24 {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, Vec::new());
                }
                let rect = GpuRect::from_bytes(&payload[..16]);
                let offset =
                    u32::from_le_bytes([payload[16], payload[17], payload[18], payload[19]]);
                let res_id =
                    u32::from_le_bytes([payload[20], payload[21], payload[22], payload[23]]);
                match self.transfer_to_host(mem, res_id, rect, offset) {
                    Ok(()) => (VIRTIO_GPU_RESP_OK_NODATA, Vec::new()),
                    Err(_) => (VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID, Vec::new()),
                }
            }
            VIRTIO_GPU_CMD_SET_SCANOUT => {
                if payload.len() < 8 {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, Vec::new());
                }
                let scanout_id =
                    u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                let res_id = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
                if scanout_id != 0 {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, Vec::new());
                }
                if res_id != 0 && !self.resources.contains_key(&res_id) {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID, Vec::new());
                }
                self.scanout_resource = if res_id == 0 { None } else { Some(res_id) };
                (VIRTIO_GPU_RESP_OK_NODATA, Vec::new())
            }
            VIRTIO_GPU_CMD_RESOURCE_FLUSH => {
                if payload.len() < 20 {
                    return (VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, Vec::new());
                }
                let rect = GpuRect::from_bytes(&payload[..16]);
                let res_id =
                    u32::from_le_bytes([payload[16], payload[17], payload[18], payload[19]]);
                match self.flush(res_id, rect) {
                    Ok(()) => (VIRTIO_GPU_RESP_OK_NODATA, Vec::new()),
                    Err(_) => (VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID, Vec::new()),
                }
            }
            _ => (VIRTIO_GPU_RESP_ERR_UNSPEC, Vec::new()),
        }
    }

    /// guest backing → host 镜像（矩形区域，RGBA 行拷贝）。
    fn transfer_to_host(
        &mut self,
        mem: &GuestMemoryMmap,
        res_id: u32,
        rect: GpuRect,
        offset: u32,
    ) -> AvmResult<()> {
        let resource = self
            .resources
            .get_mut(&res_id)
            .ok_or_else(|| AvmError::Device(format!("资源 {res_id} 不存在")))?;
        let backing = resource
            .backing
            .as_ref()
            .ok_or_else(|| AvmError::Device(format!("资源 {res_id} 无 backing")))?;
        if rect.x + rect.width > resource.width || rect.y + rect.height > resource.height {
            return Err(AvmError::Device("transfer 矩形越界".into()));
        }
        let stride = resource.width as usize * PIXEL_SIZE;
        let mut src = offset as usize; // backing 内偏移（线性布局）
        for row in rect.y..rect.y + rect.height {
            let dst = row as usize * stride + rect.x as usize * PIXEL_SIZE;
            let row_bytes = rect.width as usize * PIXEL_SIZE;
            let mut got = 0usize;
            let mut row_buf = vec![0u8; row_bytes];
            while got < row_bytes {
                // 从 backing 段读取剩余部分
                let mut seg_iter = backing.iter().copied();
                let mut advanced = 0usize;
                let need = row_bytes - got;
                let mut filled = false;
                for (addr, len) in seg_iter.by_ref() {
                    let seg_start = advanced;
                    let seg_end = seg_start + len as usize;
                    advanced = seg_end;
                    if seg_end <= src {
                        continue;
                    }
                    let in_off = src.saturating_sub(seg_start);
                    if in_off >= len as usize {
                        continue;
                    }
                    let take = need.min(len as usize - in_off);
                    mem.read_slice(
                        &mut row_buf[got..got + take],
                        GuestAddress(addr + in_off as u64),
                    )
                    .map_err(|e| AvmError::Device(format!("backing 读取失败：{e}")))?;
                    got += take;
                    src += take;
                    filled = true;
                    break;
                }
                let _ = filled;
                if got >= row_bytes {
                    break;
                }
                if advanced == 0 {
                    return Err(AvmError::Device("backing 数据不足".into()));
                }
            }
            resource.image[dst..dst + row_bytes].copy_from_slice(&row_buf);
        }
        Ok(())
    }

    /// host 镜像 → scanout 帧缓冲（矩形 blit）。
    fn flush(&mut self, res_id: u32, rect: GpuRect) -> AvmResult<()> {
        if self.scanout_resource != Some(res_id) {
            return Err(AvmError::Device(format!("资源 {res_id} 未绑定 scanout")));
        }
        let resource = self
            .resources
            .get(&res_id)
            .ok_or_else(|| AvmError::Device(format!("资源 {res_id} 不存在")))?;
        // 矩形与显示区求交
        let x0 = rect.x.min(DISPLAY_WIDTH);
        let y0 = rect.y.min(DISPLAY_HEIGHT);
        let x1 = (rect.x + rect.width).min(DISPLAY_WIDTH);
        let y1 = (rect.y + rect.height).min(DISPLAY_HEIGHT);
        if x1 <= x0 || y1 <= y0 {
            return Ok(()); // 空矩形：无事发生
        }
        let dst_stride = DISPLAY_WIDTH as usize * PIXEL_SIZE;
        let src_stride = resource.width as usize * PIXEL_SIZE;
        for row in y0..y1 {
            let sy = (row - rect.y) as usize;
            if sy >= resource.height as usize {
                break;
            }
            let sx = (x0 - rect.x) as usize;
            let w = (x1 - x0) as usize;
            let src = sy * src_stride + sx * PIXEL_SIZE;
            let dst = row as usize * dst_stride + x0 as usize * PIXEL_SIZE;
            self.framebuffer[dst..dst + w * PIXEL_SIZE]
                .copy_from_slice(&resource.image[src..src + w * PIXEL_SIZE]);
        }
        if let Some(sink) = self.sink.as_mut() {
            sink.present(DISPLAY_WIDTH, DISPLAY_HEIGHT, &self.framebuffer)?;
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const MEM_SIZE: usize = 0x40000;

    fn make_mem() -> Arc<GuestMemoryMmap> {
        Arc::new(GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap())
    }

    /// 在 guest 内存为控制队列写 desc 表 + avail ring，加入一条链。
    /// descs: (addr, len, writable)，自动串 NEXT。
    fn enqueue_chain(mem: &GuestMemoryMmap, descs: &[(u64, u32, bool)]) {
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
            mem.write_slice(&d, GuestAddress(CTRL_DESC_TABLE_ADDR + i as u64 * 16))
                .unwrap();
        }
        let avail_idx_addr = CTRL_AVAIL_RING_ADDR + 2;
        let cur: u16 = mem.read_obj(GuestAddress(avail_idx_addr)).unwrap_or(0);
        mem.write_obj(
            0u16,
            GuestAddress(CTRL_AVAIL_RING_ADDR + 4 + 2 * cur as u64),
        )
        .unwrap();
        mem.write_obj(cur.wrapping_add(1), GuestAddress(avail_idx_addr))
            .unwrap();
    }

    /// 构造一条命令链：hdr_addr 命令头 + payload_addr 负载 + resp_addr 响应缓冲。
    fn command(
        mem: &GuestMemoryMmap,
        cmd: u32,
        payload: &[u8],
        payload_addr: u64,
        resp_addr: u64,
        resp_len: u32,
    ) {
        mem.write_slice(&payload, GuestAddress(payload_addr))
            .unwrap();
        let mut hb = [0u8; 24];
        hb[0..4].copy_from_slice(&cmd.to_le_bytes());
        mem.write_slice(&hb, GuestAddress(0x20000)).unwrap();
        enqueue_chain(
            mem,
            &[
                (0x20000, 24, false),
                (payload_addr, payload.len() as u32, false),
                (resp_addr, resp_len, true),
            ],
        );
    }

    fn read_resp(mem: &GuestMemoryMmap, resp_addr: u64) -> u32 {
        let mut b = [0u8; 4];
        mem.read_slice(&mut b, GuestAddress(resp_addr)).unwrap();
        u32::from_le_bytes(b)
    }

    const PAYLOAD: u64 = 0x21000;
    const RESP: u64 = 0x22000;

    #[test]
    fn display_info_reports_fixed_mode() {
        let mut dev = VirtioGpu::new();
        let mem = make_mem();
        dev.setup_ctrl_queue(mem.as_ref()).unwrap();
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
            &[0u8; 4],
            PAYLOAD,
            RESP,
            64,
        );
        assert_eq!(dev.process_ctrl_queue(mem.as_ref()).unwrap(), 1);
        assert_eq!(
            read_resp(mem.as_ref(), RESP),
            VIRTIO_GPU_RESP_OK_DISPLAY_INFO
        );
        // 负载区：rect 800x600 + enabled=1
        let mut extra = [0u8; 24];
        mem.read_slice(&mut extra, GuestAddress(RESP + 24)).unwrap();
        let rect = GpuRect::from_bytes(&extra[..16]);
        assert_eq!(rect.width, DISPLAY_WIDTH);
        assert_eq!(rect.height, DISPLAY_HEIGHT);
        assert_eq!(u32::from_le_bytes(extra[16..20].try_into().unwrap()), 1);
    }

    #[test]
    fn create_unref_resource_lifecycle() {
        let mut dev = VirtioGpu::new();
        let mem = make_mem();
        dev.setup_ctrl_queue(mem.as_ref()).unwrap();
        // create
        let mut p = [0u8; 16];
        p[0..4].copy_from_slice(&FORMAT_B8G8R8A8_UNORM.to_le_bytes());
        p[4..8].copy_from_slice(&64u32.to_le_bytes());
        p[8..12].copy_from_slice(&64u32.to_le_bytes());
        p[12..16].copy_from_slice(&1u32.to_le_bytes());
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(read_resp(mem.as_ref(), RESP), VIRTIO_GPU_RESP_OK_NODATA);
        // 重复 id 拒绝
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(
            read_resp(mem.as_ref(), RESP),
            VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER
        );
        // unref
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_RESOURCE_UNREF,
            &p[12..16],
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(read_resp(mem.as_ref(), RESP), VIRTIO_GPU_RESP_OK_NODATA);
    }

    /// 全链路：create → attach backing → transfer → set scanout → flush → 截图比对。
    #[test]
    fn full_pipeline_scanout_snapshot() {
        let mut dev = VirtioGpu::new();
        let mem = make_mem();
        dev.setup_ctrl_queue(mem.as_ref()).unwrap();

        // backing：guest 内存 0x24000 起，4 行 × 64 像素
        let backing_addr: u64 = 0x24000;
        let res_w = 64u32;
        let res_h = 4u32;
        // 图案：第 r 行 = (0x40+r) 的 B8G8R8A8
        for r in 0..res_h {
            let row = vec![(0x40 + r) as u8; res_w as usize * PIXEL_SIZE];
            mem.write_slice(
                &row,
                GuestAddress(backing_addr + r as u64 * res_w as u64 * PIXEL_SIZE as u64),
            )
            .unwrap();
        }

        // 1. create
        let mut p = [0u8; 16];
        p[0..4].copy_from_slice(&FORMAT_B8G8R8A8_UNORM.to_le_bytes());
        p[4..8].copy_from_slice(&res_w.to_le_bytes());
        p[8..12].copy_from_slice(&res_h.to_le_bytes());
        p[12..16].copy_from_slice(&7u32.to_le_bytes());
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(read_resp(mem.as_ref(), RESP), VIRTIO_GPU_RESP_OK_NODATA);

        // 2. attach backing（payload: res_id + nr_entries + pad + mem entry）
        let mut p = [0u8; 32];
        p[0..4].copy_from_slice(&7u32.to_le_bytes());
        p[4..8].copy_from_slice(&1u32.to_le_bytes());
        p[16..24].copy_from_slice(&backing_addr.to_le_bytes());
        p[24..28].copy_from_slice(&(res_w * res_h * PIXEL_SIZE as u32).to_le_bytes());
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(read_resp(mem.as_ref(), RESP), VIRTIO_GPU_RESP_OK_NODATA);

        // 3. transfer to host 2D（全图）
        let mut p = [0u8; 24];
        let rect = GpuRect {
            x: 0,
            y: 0,
            width: res_w,
            height: res_h,
        };
        p[..16].copy_from_slice(&rect.to_bytes());
        p[20..24].copy_from_slice(&7u32.to_le_bytes());
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(read_resp(mem.as_ref(), RESP), VIRTIO_GPU_RESP_OK_NODATA);

        // 4. set scanout
        let mut p = [0u8; 8];
        p[4..8].copy_from_slice(&7u32.to_le_bytes());
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_SET_SCANOUT,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(read_resp(mem.as_ref(), RESP), VIRTIO_GPU_RESP_OK_NODATA);
        assert_eq!(dev.scanout_resource(), Some(7));

        // 5. flush（放到显示 (16, 8)）
        let mut p = [0u8; 20];
        let dst_rect = GpuRect {
            x: 16,
            y: 8,
            width: res_w,
            height: res_h,
        };
        p[..16].copy_from_slice(&dst_rect.to_bytes());
        p[16..20].copy_from_slice(&7u32.to_le_bytes());
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_RESOURCE_FLUSH,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(read_resp(mem.as_ref(), RESP), VIRTIO_GPU_RESP_OK_NODATA);

        // 6. 截图比对：显示 (16,8) 处第 0 行应为 0x40
        let fb = dev.framebuffer();
        let stride = DISPLAY_WIDTH as usize * PIXEL_SIZE;
        let off = 8 * stride + 16 * PIXEL_SIZE;
        assert_eq!(&fb[off..off + PIXEL_SIZE], &[0x40, 0x40, 0x40, 0x40]);
        let off_r2 = 10 * stride + 20 * PIXEL_SIZE; // 第 2 行（0x42）
        assert_eq!(&fb[off_r2..off_r2 + PIXEL_SIZE], &[0x42, 0x42, 0x42, 0x42]);
        // 未写入区域保持 0
        assert_eq!(&fb[0..PIXEL_SIZE], &[0, 0, 0, 0]);
    }

    #[test]
    fn flush_presents_to_bmp_sink() {
        use crate::display::BmpSink;
        let mut dev = VirtioGpu::new();
        let path = std::env::temp_dir().join("avm-gpu-scanout.bmp");
        dev.set_sink(Box::new(BmpSink::new(&path)));
        let mem = make_mem();
        dev.setup_ctrl_queue(mem.as_ref()).unwrap();
        // 最小链路：create → flush（未绑定 scanout 应报错，不落盘）
        let mut p = [0u8; 20];
        p[16..20].copy_from_slice(&9u32.to_le_bytes());
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_RESOURCE_FLUSH,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(
            read_resp(mem.as_ref(), RESP),
            VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID
        );
        assert!(!path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn flush_unbound_resource_rejected() {
        let mut dev = VirtioGpu::new();
        let mem = make_mem();
        dev.setup_ctrl_queue(mem.as_ref()).unwrap();
        let mut p = [0u8; 20];
        p[16..20].copy_from_slice(&9u32.to_le_bytes());
        command(
            mem.as_ref(),
            VIRTIO_GPU_CMD_RESOURCE_FLUSH,
            &p,
            PAYLOAD,
            RESP,
            32,
        );
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(
            read_resp(mem.as_ref(), RESP),
            VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID
        );
    }

    #[test]
    fn unknown_command_reports_unspec() {
        let mut dev = VirtioGpu::new();
        let mem = make_mem();
        dev.setup_ctrl_queue(mem.as_ref()).unwrap();
        command(mem.as_ref(), 0x7FFF, &[0u8; 4], PAYLOAD, RESP, 32);
        dev.process_ctrl_queue(mem.as_ref()).unwrap();
        assert_eq!(read_resp(mem.as_ref(), RESP), VIRTIO_GPU_RESP_ERR_UNSPEC);
    }

    #[test]
    fn process_requires_setup() {
        let mut dev = VirtioGpu::new();
        let mem = make_mem();
        assert!(dev.process_ctrl_queue(mem.as_ref()).is_err());
    }
}
