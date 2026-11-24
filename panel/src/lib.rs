//! avm-panel：NSH 纯 Rust 管理面板（M7）。
//!
//! 基于 NSH（Native Shell / Tray Rust API）。关闭按钮隐藏到托盘，
//! 实例由 avmd 继续持有，面板只是客户端视图。

use avm_client::Client;
use avm_core::AvmResult;

/// 面板状态。
#[derive(Debug)]
pub struct Panel {
    client: Client,
}

impl Panel {
    pub fn new(daemon_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(&daemon_url.into()),
        }
    }

    /// 刷新面板显示的实例列表（M7 接 NSH 真实 UI）。
    pub fn refresh(&self) -> AvmResult<Vec<String>> {
        Ok(self.client.list()?.into_iter().map(|v| v.id).collect())
    }
}

/// M7：set_tray 注册托盘图标 + 最小化隐藏。
pub fn setup_tray() -> AvmResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_builds() {
        let p = Panel::new("http://127.0.0.1:8080");
        assert!(p.refresh().is_err()); // 骨架阶段未连 daemon
    }
}
