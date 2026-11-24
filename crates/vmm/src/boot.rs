//! Linux bzImage 引导（M1）。
//!
//! 职责：
//! 1. 按 linux-loader `BzImage` 解析 bzImage 头（`HdrS` magic、版本 >= 2.00、
//!    loadflags bit0=bzImage），加载保护模式内核到 guest 内存；
//! 2. initrd 与命令行写入 guest 内存（linux-loader 0.12 不含 initrd 助手，自实现）；
//! 3. 计算 vcpu 初始寄存器（64 位长模式入口 = 内核加载地址）。
//!
//! 真机执行依赖 Windows WHPX（M0 已实装 set_regs / run_vcpu）；
//! 本模块的解析与内存布局逻辑在任意宿主可用测试验证。

use linux_loader::cmdline::Cmdline;
use linux_loader::loader::bzimage::BzImage;
use linux_loader::loader::{load_cmdline, KernelLoader, KernelLoaderResult};
use std::io::{Cursor, Read};
use vm_memory::{Address, Bytes, GuestAddress, GuestMemoryMmap};

/// x86_64 下经典 Linux 内存布局（与 QEMU pc 机保持一致，便于后续兼容）。
/// Android x86_64 引导配置（M5）。
pub mod android {
    /// Android 引导内核命令行模板（android-x86 常用参数 + 串口控制台）。
    pub const CMDLINE_TEMPLATE: &str = "console=ttyS0,115200 root=/dev/ram0 \
androidboot.hardware=android_x86_64 androidboot.selinux=permissive \
androidboot.debuggable=1 quiet loglevel=5";

    /// 生成 Android 命令行（可追加自定义参数）。
    pub fn cmdline(extra: &str) -> String {
        if extra.is_empty() {
            CMDLINE_TEMPLATE.to_string()
        } else {
            format!("{CMDLINE_TEMPLATE} {extra}")
        }
    }
}

pub mod layout {
    /// 常规内存起点。
    pub const RAM_START: u64 = 0x0;
    /// 内核加载地址（bzImage 64 位入口区）。
    pub const KERNEL_LOAD: u64 = 0x100_0000; // 16 MiB
    /// initrd 目标地址：按内核结束地址向上对齐（运行时计算，避免 512MiB guest 溢出）。
    pub const INITRD_ALIGN: u64 = 2 * 1024 * 1024; // 2 MiB
    /// 命令行地址。
    pub const CMDLINE_LOAD: u64 = 0x0_8000;
    /// zero page（boot params）地址。
    pub const ZERO_PAGE: u64 = 0x0_7000;
    /// M1 阶段最小 guest 内存（MiB）。
    pub const MIN_MEMORY_MIB: u64 = 512;
    /// cmdline 容量（含 nul）。
    pub const CMDLINE_CAPACITY: usize = 4096;
}

/// 引导所需的宿主文件集合。
#[derive(Debug, Clone)]
pub struct BootAssets {
    pub kernel_path: String,
    pub initrd_path: Option<String>,
    pub cmdline: String,
}

/// 引导完成后的 vcpu 初始状态（仅 M1 所需寄存器）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcpuBootRegs {
    pub rip: u64,
    pub rsp: u64,
    /// 直接跳 64 位入口时 CS.base=0, selector=0x08（内核 setup 自行重装段）。
    pub cs_selector: u16,
    pub cs_base: u64,
}

/// 加载结果：guest 内存 + 内核加载结果 + initrd 实际加载地址。
#[derive(Debug)]
pub struct LoadedKernel {
    pub mem: GuestMemoryMmap,
    pub kernel: KernelLoaderResult,
    pub initrd_addr: Option<GuestAddress>,
}

/// 构造 guest 内存并加载内核 + initrd + cmdline。
pub fn load_kernel(
    memory_mib: u64,
    assets: &BootAssets,
) -> Result<LoadedKernel, avm_core::AvmError> {
    use avm_core::AvmError;

    if memory_mib < layout::MIN_MEMORY_MIB {
        return Err(AvmError::Boot(format!(
            "Linux 引导至少需要 {} MiB 内存，当前 {memory_mib}",
            layout::MIN_MEMORY_MIB
        )));
    }
    if assets.cmdline.is_empty() {
        return Err(AvmError::Boot("cmdline 不能为空".into()));
    }

    let mem = GuestMemoryMmap::from_ranges(&[(
        GuestAddress(layout::RAM_START),
        (memory_mib * 1024 * 1024) as usize,
    )])
    .map_err(|e| AvmError::Boot(format!("guest 内存构造失败：{e}")))?;

    // 内核
    let mut image = Vec::new();
    std::fs::File::open(&assets.kernel_path)
        .and_then(|mut f| f.read_to_end(&mut image))
        .map_err(|e| AvmError::Boot(format!("内核文件读取失败：{e}")))?;
    let result = BzImage::load(
        &mem,
        Some(GuestAddress(layout::KERNEL_LOAD)),
        &mut Cursor::new(&image),
        None,
    )
    .map_err(|e| AvmError::Boot(format!("内核加载失败：{e}")))?;

    // initrd：紧跟内核末尾向上对齐（linux-loader 0.12 无内置助手，自实现）
    let mut initrd_addr = None;
    if let Some(path) = &assets.initrd_path {
        let mut initrd = Vec::new();
        std::fs::File::open(path)
            .and_then(|mut f| f.read_to_end(&mut initrd))
            .map_err(|e| AvmError::Boot(format!("initrd 读取失败：{e}")))?;
        let addr = (result.kernel_end / layout::INITRD_ALIGN * layout::INITRD_ALIGN)
            + layout::INITRD_ALIGN;
        let end = addr + initrd.len() as u64;
        if end >= memory_mib * 1024 * 1024 {
            return Err(AvmError::Boot(format!(
                "initrd 超出 guest 内存（end=0x{end:x}）"
            )));
        }
        mem.write_slice(&initrd, GuestAddress(addr))
            .map_err(|e| AvmError::Boot(format!("initrd 写入失败：{e}")))?;
        initrd_addr = Some(GuestAddress(addr));
    }

    // 命令行
    let mut cmdline = Cmdline::new(layout::CMDLINE_CAPACITY)
        .map_err(|e| AvmError::Boot(format!("cmdline 构造失败：{e}")))?;
    cmdline
        .insert_str(&assets.cmdline)
        .map_err(|e| AvmError::Boot(format!("cmdline 插入失败：{e}")))?;
    load_cmdline(&mem, GuestAddress(layout::CMDLINE_LOAD), &cmdline)
        .map_err(|e| AvmError::Boot(format!("cmdline 写入失败：{e}")))?;

    Ok(LoadedKernel {
        mem,
        kernel: result,
        initrd_addr,
    })
}

/// 由加载结果计算 vcpu 初始寄存器。
pub fn boot_regs(kernel: &KernelLoaderResult) -> Result<VcpuBootRegs, avm_core::AvmError> {
    Ok(VcpuBootRegs {
        rip: kernel.kernel_load.raw_value(),
        rsp: 0,
        cs_selector: 0x08,
        cs_base: 0,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn android_cmdline_preset() {
        let c = super::android::cmdline("");
        assert!(c.starts_with("console=ttyS0,115200"));
        assert!(c.contains("androidboot.hardware=android_x86_64"));
        assert!(c.contains("androidboot.selinux=permissive"));
        let c2 = super::android::cmdline("vmargin=0x1");
        assert!(c2.ends_with("vmargin=0x1"));
    }

    use super::*;
    use std::io::Write;

    /// 构造最小 bzImage：setup 扇区（合法头）+ 保护模式内核体。
    fn write_fake_bzimage(dir: &std::path::Path) -> String {
        let mut img = vec![0u8; 1024];
        // boot_flag (0x1FE) = 0xAA55
        img[0x1FE] = 0x55;
        img[0x1FF] = 0xAA;
        // setup_header 从 0x1F1 开始：setup_sects=1
        img[0x1F1] = 1;
        // header @0x202 = "HdrS"
        img[0x202..0x206].copy_from_slice(b"HdrS");
        // version @0x206 = 0x020d (2.13)
        img[0x206] = 0x0d;
        img[0x207] = 0x02;
        // loadflags @0x211：bit0 = HIGH_LOADED
        img[0x211] = 0x01;
        // code32_start @0x214 = 0x100000（会被 kernel_offset 覆盖，仅作缺省）
        img[0x214..0x218].copy_from_slice(&0x0010_0000u32.to_le_bytes());

        let path = dir.join("bzImage");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&img).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn write_initrd(dir: &std::path::Path) -> String {
        let path = dir.join("initrd.img");
        std::fs::write(&path, b"AVM-FAKE-INITRD").unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn rejects_small_memory() {
        let assets = BootAssets {
            kernel_path: "/nonexistent".into(),
            initrd_path: None,
            cmdline: "console=ttyS0".into(),
        };
        let e = load_kernel(256, &assets).unwrap_err();
        assert!(e.to_string().contains("至少需要"), "{e}");
    }

    #[test]
    fn rejects_empty_cmdline() {
        let assets = BootAssets {
            kernel_path: "/nonexistent".into(),
            initrd_path: None,
            cmdline: String::new(),
        };
        let e = load_kernel(1024, &assets).unwrap_err();
        assert!(e.to_string().contains("cmdline"), "{e}");
    }

    #[test]
    fn loads_fake_bzimage_and_computes_entry() {
        let dir = std::env::temp_dir().join("avm-m1-test");
        std::fs::create_dir_all(&dir).unwrap();
        let kernel = write_fake_bzimage(&dir);
        let initrd = write_initrd(&dir);

        let assets = BootAssets {
            kernel_path: kernel,
            initrd_path: Some(initrd),
            cmdline: "console=ttyS0 earlyprintk=serial".into(),
        };
        let loaded = load_kernel(512, &assets).unwrap();
        assert_eq!(loaded.kernel.kernel_load.raw_value(), layout::KERNEL_LOAD);
        let mem = &loaded.mem;

        // cmdline 已写入 guest 内存
        let mut buf = vec![0u8; 16];
        mem.read_slice(&mut buf, GuestAddress(layout::CMDLINE_LOAD))
            .unwrap();
        assert!(String::from_utf8_lossy(&buf).starts_with("console=ttyS0"));

        // initrd 已写入 guest 内存（紧跟内核对齐处）
        let initrd_addr = loaded.initrd_addr.expect("initrd 应已加载");
        assert!(initrd_addr.raw_value() > loaded.kernel.kernel_end);
        assert_eq!(initrd_addr.raw_value() % layout::INITRD_ALIGN, 0);
        let mut ibuf = [0u8; 6];
        mem.read_slice(&mut ibuf, initrd_addr).unwrap();
        assert_eq!(&ibuf, b"AVM-FA");

        // 寄存器推导
        let regs = boot_regs(&loaded.kernel).unwrap();
        assert_eq!(regs.rip, layout::KERNEL_LOAD);
        assert_eq!(regs.cs_selector, 0x08);
        assert_eq!(regs.cs_base, 0);
    }

    #[test]
    fn rejects_invalid_kernel_image() {
        let dir = std::env::temp_dir().join("avm-m1-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad");
        std::fs::write(&path, vec![0u8; 512]).unwrap();
        let assets = BootAssets {
            kernel_path: path.to_string_lossy().into_owned(),
            initrd_path: None,
            cmdline: "console=ttyS0".into(),
        };
        let e = load_kernel(512, &assets).unwrap_err();
        assert!(e.to_string().contains("内核加载失败"), "{e}");
    }

    #[test]
    fn rejects_initrd_overflow() {
        let dir = std::env::temp_dir().join("avm-m1-big-initrd");
        std::fs::create_dir_all(&dir).unwrap();
        let kernel = write_fake_bzimage(&dir);
        let initrd = dir.join("big");
        std::fs::write(&initrd, vec![0u8; 600 * 1024 * 1024]).unwrap();
        let assets = BootAssets {
            kernel_path: kernel,
            initrd_path: Some(initrd.to_string_lossy().into_owned()),
            cmdline: "console=ttyS0".into(),
        };
        let e = load_kernel(512, &assets).unwrap_err();
        assert!(e.to_string().contains("超出 guest 内存"), "{e}");
    }
}
