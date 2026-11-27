//! doctor：环境前置检测（`avm doctor`）。
//!
//! 检测项：WHPX 可用性、WinHvPlatform.dll、HAXM 冲突提示、KVM/HVF（M8）。

use avm_core::AvmResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckItem {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// 告警项：不影响整体通过，但报告用 [⚠] 标出
    pub warning: bool,
}

impl CheckItem {
    fn fail(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
            warning: false,
        }
    }

    fn warn(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            detail: detail.into(),
            warning: true,
        }
    }

    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            detail: detail.into(),
            warning: false,
        }
    }
}

/// GPU/Vulkan 支持检测（M9 前置告警，见 docs/PLAN-M9.md §7）。
/// 告警不失败：VMM 与 2D 自动化不需要 GPU。
pub fn check_gpu() -> CheckItem {
    let st = crate::gpucheck::detect();
    match st {
        crate::gpucheck::VulkanStatus::Full => {
            CheckItem::pass("Vulkan", crate::gpucheck::VulkanStatus::Full.detail())
        }
        other => CheckItem::warn("Vulkan 支持告警", other.detail()),
    }
}

/// 运行全部检测，返回逐项结果。
pub fn check_all() -> AvmResult<Vec<CheckItem>> {
    let mut items = Vec::new();

    if cfg!(windows) {
        #[cfg(windows)]
        {
            use crate::whp::raw;
            // 1. WinHvPlatform.dll 可加载
            items.push(if raw::library_available() {
                CheckItem::pass("WinHvPlatform.dll", "已找到")
            } else {
                CheckItem::fail(
                    "WinHvPlatform.dll",
                    "加载失败：请启用 Windows 功能「虚拟机平台」（可选功能：Hyper-V 虚拟机平台）",
                )
            });

            // 2. WHPX 分区能否创建（真实探针）
            match crate::whp::WhpHypervisor::new().and_then(|h| h.create_partition(1, 64)) {
                Ok(p) => {
                    drop(p);
                    items.push(CheckItem::pass(
                        "WHPX 分区",
                        "可创建（1 vcpu / 64MiB 探针通过）",
                    ));
                }
                Err(e) => items.push(CheckItem::fail("WHPX 分区", e.to_string())),
            }

            // 3. HAXM 冲突提示：HAXM 与 WHPX 互斥
            items.push(CheckItem::pass(
                "HAXM 冲突",
                "提示：若 Android 模拟器（HAXM）在运行，需先卸载或停用才能启用 WHPX",
            ));
        }
    } else {
        items.push(CheckItem::fail(
            "WHPX",
            "当前宿主非 Windows；WHPX 仅 Windows 可用（M8 起支持 KVM/HVF）",
        ));

        // Linux：预检 KVM 可用性（M8：API 版本探测）
        if cfg!(target_os = "linux") {
            items.push(match crate::kvm::KvmHypervisor::new() {
                Ok(h) => {
                    let _ = h;
                    CheckItem::pass("/dev/kvm", "打开成功且 API 版本 >= 12（KVM 后端可用）")
                }
                Err(e) => CheckItem::fail("/dev/kvm", &e.to_string()),
            });
        }
    }

    // GPU/Vulkan：告警级（非失败项）
    items.push(check_gpu());

    Ok(items)
}

/// 打印报告，返回整体是否通过。
pub fn report() -> AvmResult<bool> {
    let items = check_all()?;
    let mut all_ok = true;
    for it in &items {
        let mark = if !it.ok {
            "[✗]"
        } else if it.warning {
            "[⚠]"
        } else {
            "[✓]"
        };
        println!("{mark} {} —— {}", it.name, it.detail);
        if !it.ok {
            all_ok = false;
        }
    }
    Ok(all_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_all_returns_items() {
        let items = check_all().unwrap();
        assert!(!items.is_empty());
    }

    #[test]
    fn report_runs() {
        // 非 Windows 宿主必有失败项，但报告本身不 panic
        let _ = report();
    }
}
