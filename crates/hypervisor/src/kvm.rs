//! KVM 实现（M8）：Linux 平台后端，手写 ioctl FFI（与 WHPX 同风格）。
//!
//! 结构布局依赖 `kvm-bindings`（linux kvm.h 的 bindgen 结果）；ioctl 号
//! 按运行时 `sizeof` 计算，避免手算错布局。
//!
//! 非 Linux 平台：安全回退（构造报错），保证全平台可编译。
//! 真机验证需要带嵌套虚拟化（VMX/SVM）且存在 /dev/kvm 的宿主。

#![allow(dead_code)]

use std::fs::File;
use std::os::unix::io::AsRawFd;

use avm_core::{AvmError, AvmResult};
use kvm_bindings::{kvm_regs, kvm_run, kvm_segment, kvm_sregs, kvm_userspace_memory_region};

use crate::{Backend, Hypervisor, Partition, VcpuExit};

/// KVM ioctl 类型码。
const KVM_IOCTL_TYPE: u64 = 0xAE;
/// Linux uapi `_IOC` 方向位。
const IOC_NONE: u64 = 0;
const IOC_WRITE: u64 = 1;
const IOC_RDWR: u64 = 3;

/// `_IO(type, nr)`。
const fn io(nr: u64) -> u64 {
    (IOC_NONE << 30) | (KVM_IOCTL_TYPE << 8) | nr
}
/// `_IOW(type, nr, size)`。
const fn iow(nr: u64, size: u64) -> u64 {
    (IOC_WRITE << 30) | (size << 16) | (KVM_IOCTL_TYPE << 8) | nr
}
/// `_IOWR(type, nr, size)`。
const fn iowr(nr: u64, size: u64) -> u64 {
    (IOC_RDWR << 30) | (size << 16) | (KVM_IOCTL_TYPE << 8) | nr
}

/// KVM_GET_API_VERSION。
pub const KVM_GET_API_VERSION: u64 = io(0x00);
/// KVM_CREATE_VM。
pub const KVM_CREATE_VM: u64 = io(0x01);
/// KVM_CHECK_EXTENSION。
pub const KVM_CHECK_EXTENSION: u64 = io(0x03);
/// KVM_GET_VCPU_MMAP_SIZE。
pub const KVM_GET_VCPU_MMAP_SIZE: u64 = io(0x04);
/// KVM_SET_USER_MEMORY_REGION。
pub fn kvm_set_user_memory_region_ioctl() -> u64 {
    iow(
        0x40,
        std::mem::size_of::<kvm_userspace_memory_region>() as u64,
    )
}
/// KVM_CREATE_VCPU。
pub const KVM_CREATE_VCPU: u64 = io(0x41);
/// KVM_RUN。
pub const KVM_RUN: u64 = io(0x80);
/// KVM_GET_REGS。
pub fn kvm_get_regs_ioctl() -> u64 {
    iowr(0x81, std::mem::size_of::<kvm_regs>() as u64)
}
/// KVM_SET_REGS。
pub fn kvm_set_regs_ioctl() -> u64 {
    iowr(0x82, std::mem::size_of::<kvm_regs>() as u64)
}
/// KVM_GET_SREGS。
pub fn kvm_get_sregs_ioctl() -> u64 {
    iowr(0x83, std::mem::size_of::<kvm_sregs>() as u64)
}
/// KVM_SET_SREGS。
pub fn kvm_set_sregs_ioctl() -> u64 {
    iowr(0x84, std::mem::size_of::<kvm_sregs>() as u64)
}

/// 退出原因（linux kvm.h）。
const KVM_EXIT_HLT: u32 = 12;
const KVM_EXIT_IO: u32 = 2;
const KVM_EXIT_MMIO: u32 = 34;

fn sys_err(ctx: &str) -> AvmError {
    AvmError::Hypervisor(format!(
        "KVM {ctx} 失败：{}（errno {}）",
        std::io::Error::last_os_error(),
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    ))
}

/// 裸 ioctl（成功返回返回值）。
fn ioctl(fd: i32, req: u64, arg: u64) -> AvmResult<i64> {
    let r = unsafe { libc::ioctl(fd, req as libc::c_ulong, arg) };
    if r < 0 {
        Err(sys_err("ioctl"))
    } else {
        Ok(r as i64)
    }
}

/// KVM 后端：持有 /dev/kvm 文件句柄。
pub struct KvmHypervisor {
    dev: File,
}

impl KvmHypervisor {
    /// 打开 /dev/kvm 并校验 API 版本（>= 12）。
    pub fn new() -> AvmResult<Self> {
        let dev = File::open("/dev/kvm")
            .map_err(|e| AvmError::Hypervisor(format!("打开 /dev/kvm 失败：{e}")))?;
        let ver = ioctl(dev.as_raw_fd(), KVM_GET_API_VERSION, 0)?;
        if ver < 12 {
            return Err(AvmError::Hypervisor(format!(
                "KVM API 版本过旧：{ver}（需要 >= 12）"
            )));
        }
        Ok(Self { dev })
    }
}

impl Hypervisor for KvmHypervisor {
    fn backend(&self) -> Backend {
        Backend::KVM
    }

    fn create_partition(&self, vcpus: u32, memory_mib: u64) -> AvmResult<Box<dyn Partition>> {
        if vcpus == 0 || vcpus > 32 {
            return Err(AvmError::Config(format!("vcpus={vcpus} 超出 [1,32]")));
        }
        let fd = self.dev.as_raw_fd();
        let vm_fd = ioctl(fd, KVM_CREATE_VM, 0)? as i32;

        // guest 物理内存：宿主侧匿名 mmap 一整块，slot 0 从 gpa 0 映射
        let mem_size = (memory_mib * 1024 * 1024) as usize;
        let host_mem = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mem_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if host_mem == libc::MAP_FAILED {
            return Err(sys_err("mmap guest ram"));
        }
        let region = kvm_userspace_memory_region {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: mem_size as u64,
            userspace_addr: host_mem as u64,
        };
        let region_ptr = &region as *const kvm_userspace_memory_region as u64;
        ioctl(vm_fd, kvm_set_user_memory_region_ioctl(), region_ptr)?;

        // vcpu fd 列表
        let mut vcpu_fds = Vec::new();
        let mut run_maps = Vec::new();
        let mmap_size = ioctl(fd, KVM_GET_VCPU_MMAP_SIZE, 0)? as usize;
        for i in 0..vcpus {
            let vfd = ioctl(vm_fd, KVM_CREATE_VCPU, i as u64)? as i32;
            let m = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    mmap_size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    vfd,
                    0,
                )
            };
            if m == libc::MAP_FAILED {
                return Err(sys_err("mmap kvm_run"));
            }
            vcpu_fds.push(vfd);
            run_maps.push((m as *mut kvm_run, mmap_size));
        }

        Ok(Box::new(KvmPartition {
            vm_fd,
            host_mem: host_mem as *mut u8,
            mem_size,
            vcpu_fds,
            run_maps,
        }))
    }
}

/// KVM 分区：vm fd + 宿主侧 guest RAM + vcpu fds。
pub struct KvmPartition {
    vm_fd: i32,
    host_mem: *mut u8,
    mem_size: usize,
    vcpu_fds: Vec<i32>,
    run_maps: Vec<(*mut kvm_run, usize)>,
}

unsafe impl Send for KvmPartition {}

impl Drop for KvmPartition {
    fn drop(&mut self) {
        unsafe {
            for (m, size) in &self.run_maps {
                let _ = libc::munmap(*m as *mut libc::c_void, *size);
            }
            let _ = libc::munmap(self.host_mem as *mut libc::c_void, self.mem_size);
        }
        // fd 由闭包外的 File/CloseOnDrop 处理；vm_fd/vcpu_fds 手动 close
        unsafe {
            libc::close(self.vm_fd);
            for fd in &self.vcpu_fds {
                libc::close(*fd);
            }
        }
    }
}

/// 真模式代码段：selector 0、base 0、limit 0xFFFF、可读代码。
fn real_mode_cs() -> kvm_segment {
    kvm_segment {
        base: 0,
        limit: 0xFFFF,
        selector: 0,
        type_: 11, // code, readable, accessed
        present: 1,
        dpl: 0,
        db: 0,
        s: 1,
        l: 0,
        g: 0,
        avl: 0,
        unusable: 0,
        padding: 0,
    }
}

impl Partition for KvmPartition {
    fn map_memory(&mut self, gpa: u64, host: &mut [u8]) -> AvmResult<()> {
        let start = gpa as usize;
        if start + host.len() > self.mem_size {
            return Err(AvmError::Hypervisor(format!(
                "映射越界：gpa=0x{gpa:x} len={} > mem={}",
                host.len(),
                self.mem_size
            )));
        }
        let base = unsafe { self.host_mem.add(start) };
        unsafe {
            std::ptr::copy_nonoverlapping(host.as_ptr(), base, host.len());
        }
        Ok(())
    }

    fn run_vcpu(&mut self, vcpu: u32) -> AvmResult<VcpuExit> {
        let idx = vcpu as usize;
        let fd = *self
            .vcpu_fds
            .get(idx)
            .ok_or_else(|| AvmError::Hypervisor(format!("vcpu {vcpu} 不存在")))?;
        ioctl(fd, KVM_RUN, 0)?;
        let (run, _size) = self.run_maps[idx];
        let exit_reason = unsafe { (*run).exit_reason };
        match exit_reason {
            KVM_EXIT_HLT => Ok(VcpuExit::Halt),
            KVM_EXIT_IO => {
                let io = unsafe { &(*run).__bindgen_anon_1.io };
                Ok(VcpuExit::Io {
                    port: io.port,
                    size: io.size,
                    data: 0, // 帧数据在 kvm_run data_offset 处，Io 循环层按需读取
                })
            }
            KVM_EXIT_MMIO => {
                let mmio = unsafe { &(*run).__bindgen_anon_1.mmio };
                Ok(VcpuExit::Mmio {
                    gpa: mmio.phys_addr,
                    size: mmio.len as u8,
                })
            }
            other => Ok(VcpuExit::Unknown(other)),
        }
    }

    fn set_regs(&mut self, vcpu: u32, rip: u64, rsp: u64) -> AvmResult<()> {
        let idx = vcpu as usize;
        let fd = *self
            .vcpu_fds
            .get(idx)
            .ok_or_else(|| AvmError::Hypervisor(format!("vcpu {vcpu} 不存在")))?;
        // regs：rip/rsp
        let mut regs: kvm_regs = Default::default();
        ioctl(fd, kvm_get_regs_ioctl(), &mut regs as *mut _ as u64)?;
        regs.rip = rip;
        regs.rsp = rsp;
        ioctl(fd, kvm_set_regs_ioctl(), &regs as *const _ as u64)?;

        // sregs：真模式 CS base/selector 归零（toy guest 载入 0x7c00）
        let mut sregs: kvm_sregs = Default::default();
        ioctl(fd, kvm_get_sregs_ioctl(), &mut sregs as *mut _ as u64)?;
        sregs.cs = real_mode_cs();
        sregs.ss.base = 0;
        sregs.ss.selector = 0;
        sregs.ss.limit = 0xFFFF;
        ioctl(fd, kvm_set_sregs_ioctl(), &sregs as *const _ as u64)?;
        Ok(())
    }
}

/// 读取 KVM_EXIT_IO 的实际数据（kvm_run 内 data_offset 处）。
pub fn read_io_data(run: *mut kvm_run) -> AvmResult<u32> {
    let io = unsafe { &(*run).__bindgen_anon_1.io };
    if io.direction != 1 {
        return Err(AvmError::Hypervisor("KVM_EXIT_IO 非 OUT 方向".into()));
    }
    if io.size > 4 {
        return Err(AvmError::Hypervisor(format!(
            "IO 宽度 {} 超过 4 字节",
            io.size
        )));
    }
    let mut v = 0u32;
    unsafe {
        let base = (run as *mut u8).add(io.data_offset as usize);
        for i in 0..io.size as usize {
            v |= (*base.add(i) as u32) << (i * 8);
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ioctl 编码对照 linux uapi 常量。
    #[test]
    fn ioctl_numbers_match_kernel_uapi() {
        assert_eq!(KVM_GET_API_VERSION, 0xAE00);
        assert_eq!(KVM_CREATE_VM, 0xAE01);
        assert_eq!(KVM_CHECK_EXTENSION, 0xAE03);
        assert_eq!(KVM_GET_VCPU_MMAP_SIZE, 0xAE04);
        assert_eq!(KVM_CREATE_VCPU, 0xAE41);
        assert_eq!(KVM_RUN, 0xAE80);
        // 带结构体的：只看方向/类型/序号位
        let r = kvm_set_user_memory_region_ioctl();
        assert_eq!(r >> 8 & 0xFF, 0xAE);
        assert_eq!(r & 0xFF, 0x40);
        assert_eq!(r >> 30, IOC_WRITE);
        assert_eq!(kvm_get_regs_ioctl() & 0xFF, 0x81);
        assert_eq!(kvm_get_sregs_ioctl() & 0xFF, 0x83);
        assert_eq!(kvm_set_sregs_ioctl() & 0xFF, 0x84);
    }

    /// 本环境（无 /dev/kvm）应优雅报错而非 panic。
    #[test]
    fn new_fails_gracefully_without_dev_kvm() {
        match KvmHypervisor::new() {
            Ok(_) => {
                // 有 KVM 的宿主：能打开即验证 API 版本通过
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("/dev/kvm") || msg.contains("KVM"),
                    "报错应可诊断：{msg}"
                );
            }
        }
    }

    #[test]
    fn real_mode_cs_values() {
        let cs = real_mode_cs();
        assert_eq!(cs.base, 0);
        assert_eq!(cs.selector, 0);
        assert_eq!(cs.limit, 0xFFFF);
        assert_eq!(cs.present, 1);
        assert_eq!(cs.s, 1);
    }
}
