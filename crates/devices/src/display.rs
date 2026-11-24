//! 帧呈现（M4b）：virtio-gpu scanout 帧缓冲的呈现出口。
//!
//! `FrameSink` 抽象呈现后端：
//! - `CaptureSink`：内存捕获（测试/HTTP API 取图）
//! - `BmpSink`：每次 present 落盘 BMP（纯 std，真机窗口呈现待 minifb 接入）
//!
//! 每次 `RESOURCE_FLUSH` 后 VirtioGpu 会调用 sink。

use std::path::PathBuf;

use avm_core::AvmResult;

use crate::bmp::write_bmp;

/// 呈现后端：收到 scanout 帧缓冲（BGRA 逐行、自顶向下）。
pub trait FrameSink: Send {
    fn present(&mut self, width: u32, height: u32, pixels_bgra: &[u8]) -> AvmResult<()>;
}

/// 内存捕获：保留最近一帧（测试/截图 API 用）。
#[derive(Default)]
pub struct CaptureSink {
    pub last: Option<(u32, u32, Vec<u8>)>,
    /// present 次数。
    pub count: usize,
}

impl FrameSink for CaptureSink {
    fn present(&mut self, width: u32, height: u32, pixels_bgra: &[u8]) -> AvmResult<()> {
        self.last = Some((width, height, pixels_bgra.to_vec()));
        self.count += 1;
        Ok(())
    }
}

/// BMP 落盘：每次 present 覆盖写一个 BMP 文件。
pub struct BmpSink {
    path: PathBuf,
}

impl BmpSink {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl FrameSink for BmpSink {
    fn present(&mut self, width: u32, height: u32, pixels_bgra: &[u8]) -> AvmResult<()> {
        write_bmp(&self.path, width, height, pixels_bgra)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_sink_keeps_last_frame() {
        let mut sink = CaptureSink::default();
        sink.present(4, 4, &[1u8; 64]).unwrap();
        sink.present(2, 2, &[2u8; 16]).unwrap();
        assert_eq!(sink.count, 2);
        let (w, h, px) = sink.last.unwrap();
        assert_eq!((w, h, px.len()), (2, 2, 16));
    }

    #[test]
    fn bmp_sink_writes_file() {
        let path = std::env::temp_dir().join("avm-bmp-sink.bmp");
        let mut sink = BmpSink::new(&path);
        sink.present(2, 2, &[3u8; 16]).unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_eq!(b.len(), 54 + 16);
        let _ = std::fs::remove_file(&path);
    }
}
