//! WHPX FFI（M0）。
//!
//! Windows 宿主：链接 WinHvPlatform.dll，真实调用 WHPX API；
//! 其它平台：编译为安全回退（构造报错），保证 workspace 全平台可编译。
//!
//! 参考 libkrun `src/whp/` 与 Microsoft WHP 文档：
//! https://learn.microsoft.com/en-us/virtualization/api/

#![allow(
    non_snake_case,
    dead_code,
    non_camel_case_types,
    non_upper_case_globals,
    unreachable_code
)]

use crate::{Backend, Hypervisor, Partition, VcpuExit};
use avm_core::{AvmError, AvmResult};

// ---------------------------------------------------------------------------
// 类型与常量
// ---------------------------------------------------------------------------

/// WHP API 返回码，0 = S_OK。
pub type HRESULT = i32;
pub type WHV_PARTITION_HANDLE = *mut core::ffi::c_void;

/// 分区属性代码（winhvplatform.h）。
pub const WHV_PARTITION_PROPERTY_CODE_PROCESSOR_COUNT: u32 = 0x0002;
pub const WHV_PARTITION_PROPERTY_CODE_MEMORY_BYTES: u32 = 0x0007;

/// 分区属性联合的最小表示（我们只用第一个 u64 字段）。
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WhvPartitionProperty {
    pub value: u64,
    pub reserved: [u64; 7],
}

/// GPR / 段寄存器名子集（winhvemulation）。
pub const WHvX64RegisterRax: u32 = 0x00004000;
pub const WHvX64RegisterRip: u32 = 0x00004002;
pub const WHvX64RegisterRsp: u32 = 0x00004003;
pub const WHvX64RegisterCs: u32 = 0x00005002;

/// 段寄存器描述。
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WhvX64SegmentRegister {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    pub attributes: u16,
}

/// 退出原因子集。
pub const WHV_RUN_VP_EXIT_REASON_X64_HALT: u32 = 0x0010;
pub const WHV_RUN_VP_EXIT_REASON_MEMORY_ACCESS: u32 = 0x0002;
pub const WHV_RUN_VP_EXIT_REASON_X64_IO_PORT_ACCESS: u32 = 0x0006;
pub const WHV_RUN_VP_EXIT_REASON_X64_MMIO_ACCESS: u32 = 0x0007;

/// vcpu 退出上下文。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WhvRunVpExitContext {
    pub exit_reason: u32,
    pub reserved: u32,
    /// 对 IO/MMIO 退出：`VPContext` 之后才是具体访问信息结构，
    /// M0 实装时从详细上下文读取 port / access size / rax。
    pub detail: [u64; 8],
}

impl Default for WhvRunVpExitContext {
    fn default() -> Self {
        Self {
            exit_reason: 0,
            reserved: 0,
            detail: [0; 8],
        }
    }
}

// ---------------------------------------------------------------------------
// 原始 FFI（仅 Windows 编译进二进制）
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod raw {
    use super::*;

    #[link(name = "WinHvPlatform")]
    extern "system" {
        pub fn WinHvCreatePartition(handle: *mut WHV_PARTITION_HANDLE) -> HRESULT;
        pub fn WinHvDeletePartition(handle: WHV_PARTITION_HANDLE) -> HRESULT;
        pub fn WinHvSetPartitionProperty(
            handle: WHV_PARTITION_HANDLE,
            code: u32,
            value: WhvPartitionProperty,
        ) -> HRESULT;
        pub fn WinHvMapGpaRange(
            handle: WHV_PARTITION_HANDLE,
            source: *mut u8,
            gpa: u64,
            size: u64,
            flags: u32,
        ) -> HRESULT;
        pub fn WinHvCreateVirtualProcessor(
            handle: WHV_PARTITION_HANDLE,
            index: u32,
            flags: u32,
        ) -> HRESULT;
        pub fn WinHvRunVirtualProcessor(
            handle: WHV_PARTITION_HANDLE,
            index: u32,
            context: *mut WhvRunVpExitContext,
            size: u32,
        ) -> HRESULT;
        pub fn WinHvSetVirtualProcessorRegisters(
            handle: WHV_PARTITION_HANDLE,
            index: u32,
            names: *const u32,
            values: *const u64,
            count: u32,
        ) -> HRESULT;
        pub fn WinHvSetVirtualProcessorReg(
            handle: WHV_PARTITION_HANDLE,
            index: u32,
            name: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> HRESULT;
    }

    /// 是否能加载 WinHvPlatform.dll（doctor / 运行前检测）。
    pub fn library_available() -> bool {
        unsafe {
            let h = LoadLibraryA(b"WinHvPlatform.dll\0".as_ptr());
            if h.is_null() {
                false
            } else {
                FreeLibrary(h);
                true
            }
        }
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const u8) -> *mut core::ffi::c_void;
        fn FreeLibrary(lib: *mut core::ffi::c_void) -> i32;
    }
}

pub fn hr_ok(api: &str, hr: HRESULT) -> AvmResult<()> {
    if hr == 0 {
        Ok(())
    } else {
        Err(AvmError::Hypervisor(format!(
            "{api} 失败 HRESULT=0x{hr:08x}"
        )))
    }
}

// ---------------------------------------------------------------------------
// 安全封装（Windows 实装 / 非 Windows 回退）
// ---------------------------------------------------------------------------

/// WHPX 后端。Windows 上加载 WinHvPlatform.dll，其它平台构造即报错。
#[derive(Debug, Default)]
pub struct WhpHypervisor;

impl WhpHypervisor {
    pub fn new() -> AvmResult<Self> {
        if cfg!(windows) {
            #[cfg(windows)]
            if !raw::library_available() {
                return Err(AvmError::Hypervisor(
                    "WinHvPlatform.dll 加载失败：请启用 Windows 功能「虚拟机平台」（doctor 检测）"
                        .into(),
                ));
            }
            Ok(Self)
        } else {
            Err(AvmError::Hypervisor(
                "WHPX 仅在 Windows 宿主可用（M8 的 KVM/HVF 同 trait 跨平台）".into(),
            ))
        }
    }
}

impl Hypervisor for WhpHypervisor {
    fn backend(&self) -> Backend {
        Backend::WHPX
    }

    fn create_partition(&self, vcpus: u32, memory_mib: u64) -> AvmResult<Box<dyn Partition>> {
        #[cfg(not(windows))]
        {
            let _ = (vcpus, memory_mib);
            return Err(AvmError::Hypervisor("WHPX 仅在 Windows 宿主可用".into()));
        }

        #[cfg(windows)]
        unsafe {
            let mut handle: WHV_PARTITION_HANDLE = std::ptr::null_mut();
            hr_ok(
                "WinHvCreatePartition",
                raw::WinHvCreatePartition(&mut handle),
            )?;

            let mut prop = WhvPartitionProperty {
                value: vcpus as u64,
                ..Default::default()
            };
            hr_ok(
                "WinHvSetPartitionProperty(PROCESSOR_COUNT)",
                raw::WinHvSetPartitionProperty(
                    handle,
                    WHV_PARTITION_PROPERTY_CODE_PROCESSOR_COUNT,
                    prop,
                ),
            )?;

            prop.value = memory_mib * 1024 * 1024;
            hr_ok(
                "WinHvSetPartitionProperty(MEMORY_BYTES)",
                raw::WinHvSetPartitionProperty(
                    handle,
                    WHV_PARTITION_PROPERTY_CODE_MEMORY_BYTES,
                    prop,
                ),
            )?;

            for i in 0..vcpus {
                hr_ok(
                    "WinHvCreateVirtualProcessor",
                    raw::WinHvCreateVirtualProcessor(handle, i, 0),
                )?;
            }

            Ok(Box::new(WhpPartition {
                handle,
                vcpus,
                _memory_mib: memory_mib,
            }))
        }
    }
}

/// WHPX Partition 句柄。
pub struct WhpPartition {
    handle: WHV_PARTITION_HANDLE,
    vcpus: u32,
    _memory_mib: u64,
}

// WHPX 句柄可跨线程使用（WHP API 无线程亲和性要求）。
unsafe impl Send for WhpPartition {}

// Drop 释放分区（Windows）。
impl Drop for WhpPartition {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            let _ = raw::WinHvDeletePartition(self.handle);
        }
    }
}

impl Partition for WhpPartition {
    fn map_memory(&mut self, gpa: u64, host: &mut [u8]) -> AvmResult<()> {
        #[cfg(not(windows))]
        {
            let _ = (gpa, host);
            return Err(AvmError::Hypervisor("WHPX 仅在 Windows 宿主可用".into()));
        }

        #[cfg(windows)]
        unsafe {
            // WHV_MAP_GPA_RANGE_FLAG_READ(1) | WRITE(2) | EXECUTABLE(4)
            const RWX: u32 = 1 | 2 | 4;
            let (ptr, len) = (host.as_mut_ptr(), host.len() as u64);
            hr_ok(
                "WinHvMapGpaRange",
                raw::WinHvMapGpaRange(self.handle, ptr, gpa, len, RWX),
            )?;
        }

        Ok(())
    }

    fn run_vcpu(&mut self, vcpu: u32) -> AvmResult<VcpuExit> {
        if vcpu >= self.vcpus {
            return Err(AvmError::Hypervisor(format!(
                "vcpu {vcpu} 超出分区范围（{}）",
                self.vcpus
            )));
        }
        #[cfg(not(windows))]
        {
            return Err(AvmError::Hypervisor("WHPX 仅在 Windows 宿主可用".into()));
        }

        #[cfg(windows)]
        unsafe {
            let mut ctx = WhvRunVpExitContext::default();
            hr_ok(
                "WinHvRunVirtualProcessor",
                raw::WinHvRunVirtualProcessor(
                    self.handle,
                    vcpu,
                    &mut ctx,
                    std::mem::size_of::<WhvRunVpExitContext>() as u32,
                ),
            )?;
            // M0：从 exit 上下文 detail 区拆 port/access 信息（真机验证时细化）
            Ok(match ctx.exit_reason {
                WHV_RUN_VP_EXIT_REASON_X64_HALT => VcpuExit::Halt,
                WHV_RUN_VP_EXIT_REASON_X64_IO_PORT_ACCESS => VcpuExit::Io {
                    port: (ctx.detail[0] & 0xffff) as u16,
                    size: ((ctx.detail[0] >> 16) & 0xff) as u8,
                    data: (ctx.detail[0] >> 24) as u32,
                },
                WHV_RUN_VP_EXIT_REASON_X64_MMIO_ACCESS => VcpuExit::Mmio {
                    gpa: ctx.detail[0],
                    size: 4,
                },
                other => VcpuExit::Unknown(other),
            })
        }
    }

    fn set_regs(&mut self, vcpu: u32, rip: u64, rsp: u64) -> AvmResult<()> {
        if vcpu >= self.vcpus {
            return Err(AvmError::Hypervisor(format!("vcpu {vcpu} 超出分区范围")));
        }
        #[cfg(not(windows))]
        {
            let _ = (vcpu, rip, rsp);
            return Err(AvmError::Hypervisor("WHPX 仅在 Windows 宿主可用".into()));
        }

        #[cfg(windows)]
        unsafe {
            hr_ok(
                "WinHvSetVirtualProcessorReg(RIP)",
                raw::WinHvSetVirtualProcessorReg(
                    self.handle,
                    vcpu,
                    WHvX64RegisterRip,
                    &rip as *const u64 as *const core::ffi::c_void,
                    8,
                ),
            )?;
            hr_ok(
                "WinHvSetVirtualProcessorReg(RSP)",
                raw::WinHvSetVirtualProcessorReg(
                    self.handle,
                    vcpu,
                    WHvX64RegisterRsp,
                    &rsp as *const u64 as *const core::ffi::c_void,
                    8,
                ),
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_windows_host_is_rejected() {
        if !cfg!(windows) {
            assert!(WhpHypervisor::new().is_err());
            let h = WhpHypervisor;
            assert!(h.create_partition(1, 64).is_err());
        }
    }

    #[test]
    fn hr_ok_maps_hresult() {
        assert!(hr_ok("api", 0).is_ok());
        let err = hr_ok("api", -1).unwrap_err().to_string();
        assert!(err.contains("0xffffffff"), "{err}");
    }

    #[test]
    fn exit_context_default_zero() {
        let c = WhvRunVpExitContext::default();
        assert_eq!(c.exit_reason, 0);
    }
}
