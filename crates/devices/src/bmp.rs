//! BMP 写出（M4b）：纯 std 实现 32bpp BI_RGB BMP，无外部依赖。
//!
//! 布局：BITMAPFILEHEADER(14B) + BITMAPINFOHEADER(40B) + 像素数据
//! （自底向上行序，BGRA）。

use std::fs;
use std::path::Path;

use avm_core::{AvmError, AvmResult};

/// 将 BGRA 像素（逐行、自顶向下）写成 32bpp BMP 文件。
pub fn write_bmp(path: &Path, width: u32, height: u32, pixels_bgra: &[u8]) -> AvmResult<()> {
    let w = width as usize;
    let h = height as usize;
    if pixels_bgra.len() != w * h * 4 {
        return Err(AvmError::Device(format!(
            "像素缓冲大小不符：期望 {} 字节，实际 {}",
            w * h * 4,
            pixels_bgra.len()
        )));
    }
    let row = w * 4;
    let data_size = row * h;
    let file_size = 54 + data_size;

    let mut out = Vec::with_capacity(file_size);
    // BITMAPFILEHEADER
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // 保留
    out.extend_from_slice(&54u32.to_le_bytes()); // 像素数据偏移
                                                 // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(-(height as i32)).to_le_bytes()); // 负高 = 自顶向下
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(data_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // X ppm
    out.extend_from_slice(&0u32.to_le_bytes()); // Y ppm
    out.extend_from_slice(&0u32.to_le_bytes()); // 色表
    out.extend_from_slice(&0u32.to_le_bytes()); // 重要色
                                                // 像素：BI_RGB 无填充（32bpp 天然 4 字节对齐）
    out.extend_from_slice(pixels_bgra);

    fs::write(path, &out)
        .map_err(|e| AvmError::Device(format!("BMP 写入失败 {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_header_and_pixels() {
        let dir = std::env::temp_dir().join("avm-bmp-test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("t.bmp");
        // 2x2 像素
        let mut pixels = vec![0u8; 16];
        pixels[0..4].copy_from_slice(&[0x10, 0x20, 0x30, 0xFF]); // 第1像素 B=0x10 G=0x20 R=0x30
        pixels[4..8].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        write_bmp(&path, 2, 2, &pixels).unwrap();
        let b = fs::read(&path).unwrap();
        assert_eq!(&b[..2], b"BM");
        assert_eq!(b.len(), 54 + 16);
        let data_off = u32::from_le_bytes(b[10..14].try_into().unwrap());
        assert_eq!(data_off, 54);
        let width = i32::from_le_bytes(b[18..22].try_into().unwrap());
        let height = i32::from_le_bytes(b[22..26].try_into().unwrap());
        assert_eq!((width, height), (2, -2)); // 负高 = 自顶向下
        assert_eq!(u16::from_le_bytes(b[28..30].try_into().unwrap()), 32);
        // 像素原样（BGRA，无行填充）
        assert_eq!(&b[54..70], &pixels[..]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn size_mismatch_rejected() {
        let r = write_bmp(
            &std::env::temp_dir().join("avm-bmp-bad.bmp"),
            2,
            2,
            &[0u8; 4],
        );
        assert!(r.is_err());
    }
}
