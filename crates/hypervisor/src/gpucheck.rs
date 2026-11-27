//! gpucheck：宿主 Vulkan 支持检测（M9 前置告警，见 docs/PLAN-M9.md §7）。
//!
//! 只做检测与告警，不安装、不依赖 GPU 运行 VMM：
//! - Full       → L0 宿主 Vulkan ICD 直通
//! - LoaderOnly → 有 loader 无可见 ICD（可能 driver store 未见，按 L2 处理并告警）
//! - Missing    → 连 loader 都没有 → M9 降级 ANGLE(L1)/软件 Vulkan(L2)/2D(L3)

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VulkanStatus {
    Full,
    LoaderOnly,
    Missing,
}

impl VulkanStatus {
    pub fn detail(self) -> &'static str {
        match self {
            VulkanStatus::Full => "宿主 Vulkan 可用（L0 直通）",
            VulkanStatus::LoaderOnly => {
                "检测到 Vulkan loader 但未见 ICD；建议确认显卡驱动，或走 ANGLE(L1)/软件渲染(L2)"
            }
            VulkanStatus::Missing => {
                "未检测到 Vulkan：M9 将按 PLAN-M9 §7 降级——ANGLE→D3D(L1) 或 SwiftShader/lavapipe(L2)，2D 保底(L3)；VMM 与 2D 自动化不受影响"
            }
        }
    }
}

fn first_existing(paths: &[&str]) -> bool {
    paths.iter().any(|p| std::path::Path::new(p).exists())
}

fn dir_has_json(dir: &str) -> bool {
    std::path::Path::new(dir)
        .read_dir()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .any(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        })
        .unwrap_or(false)
}

/// 检测宿主 Vulkan 支持（loader + ICD）。
pub fn detect() -> VulkanStatus {
    if cfg!(windows) {
        // loader：System32\vulkan-1.dll（Vulkan 运行时随显卡驱动/SDK 安装）
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let loader = PathBuf::from(windir).join("System32").join("vulkan-1.dll");
        if !loader.exists() {
            return VulkanStatus::Missing;
        }
        // Windows 驱动 ICD json 在 driver store，无法廉价枚举；有 loader 即视为可用
        VulkanStatus::Full
    } else if cfg!(target_os = "linux") {
        let loader = first_existing(&[
            "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
            "/usr/lib64/libvulkan.so.1",
            "/usr/lib/libvulkan.so.1",
        ]);
        let icd = dir_has_json("/usr/share/vulkan/icd.d") || dir_has_json("/etc/vulkan/icd.d");
        match (loader, icd) {
            (true, true) => VulkanStatus::Full,
            (true, false) => VulkanStatus::LoaderOnly,
            _ => VulkanStatus::Missing,
        }
    } else {
        VulkanStatus::Missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_a_status() {
        // 只要求不 panic、语义自洽
        let s = detect();
        assert!(
            s == VulkanStatus::Full || s == VulkanStatus::LoaderOnly || s == VulkanStatus::Missing
        );
    }

    #[test]
    fn details_are_nonempty() {
        assert!(!VulkanStatus::Missing.detail().is_empty());
        assert!(!VulkanStatus::LoaderOnly.detail().is_empty());
        assert!(!VulkanStatus::Full.detail().is_empty());
    }
}
