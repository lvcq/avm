//! avm-panel：管理面板（M7）。
//!
//! 面板 = 视图模型 + 托盘菜单模型（纯数据、可测试）；NSH 原生托盘/窗口壳
//! 在 Windows 真机阶段接线（feature `nsh`，当前为 cfg 存根）。
//! 关闭按钮隐藏到托盘：实例由 avmd 继续持有，面板只是客户端视图。

use avm_client::Client;
use avm_core::AvmResult;

/// 单行实例视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmRow {
    pub id: String,
    pub state: String,
    pub vcpus: u32,
    pub memory_mib: u64,
}

/// 面板视图（一次 refresh 的快照）。
#[derive(Debug, Clone, Default)]
pub struct PanelView {
    pub rows: Vec<VmRow>,
}

impl PanelView {
    /// 渲染为对齐文本表（NSH UI 壳 / 控制台共用）。
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.rows.is_empty() {
            out.push_str("（无实例）\n");
            return out;
        }
        let id_w = self
            .rows
            .iter()
            .map(|r| r.id.len())
            .max()
            .unwrap_or(2)
            .max(2);
        out.push_str(&format!(
            "{:<id_w$}  {:<8}  {:>5}  {:>9}\n",
            "ID",
            "STATE",
            "VCPUS",
            "MEM(MiB)",
            id_w = id_w
        ));
        for r in &self.rows {
            out.push_str(&format!(
                "{:<id_w$}  {:<8}  {:>5}  {:>9}\n",
                r.id,
                r.state,
                r.vcpus,
                r.memory_mib,
                id_w = id_w
            ));
        }
        out
    }
}

/// 托盘菜单模型（NSH `set_tray` 的数据源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenu {
    pub items: Vec<TrayItem>,
}

/// 托盘菜单项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayItem {
    pub label: String,
    /// 触发动作（面板命令标识）。
    pub action: TrayAction,
}

/// 托盘动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// 刷新实例列表。
    Refresh,
    /// 显示主窗口。
    ShowWindow,
    /// 退出面板（avmd 继续持有实例）。
    Quit,
}

impl TrayMenu {
    /// 默认托盘菜单：显示 / 刷新 / 退出。
    pub fn default_menu() -> Self {
        Self {
            items: vec![
                TrayItem {
                    label: "显示面板".into(),
                    action: TrayAction::ShowWindow,
                },
                TrayItem {
                    label: "刷新".into(),
                    action: TrayAction::Refresh,
                },
                TrayItem {
                    label: "退出".into(),
                    action: TrayAction::Quit,
                },
            ],
        }
    }

    pub fn labels(&self) -> Vec<String> {
        self.items.iter().map(|i| i.label.clone()).collect()
    }
}

/// 面板：daemon 客户端 + 视图缓存。
#[derive(Debug)]
pub struct Panel {
    client: Client,
    view: Option<PanelView>,
}

impl Panel {
    pub fn new(daemon_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(&daemon_url.into()),
            view: None,
        }
    }

    /// 当前视图（未 refresh 时为 None）。
    pub fn view(&self) -> Option<&PanelView> {
        self.view.as_ref()
    }

    /// 刷新实例列表并缓存视图（返回快照）。
    pub fn refresh(&mut self) -> AvmResult<PanelView> {
        let rows = self
            .client
            .list()?
            .into_iter()
            .map(|v| VmRow {
                id: v.id,
                state: v.state,
                vcpus: v.vcpus,
                memory_mib: v.memory_mib,
            })
            .collect();
        let view = PanelView { rows };
        self.view = Some(view.clone());
        Ok(view)
    }

    /// 创建实例并刷新视图。
    pub fn create(&mut self, id: &str, vcpus: u32, memory_mib: u64) -> AvmResult<()> {
        self.client.create(id, vcpus, memory_mib)?;
        self.refresh()?;
        Ok(())
    }

    /// 引导实例并刷新视图。
    pub fn boot(&mut self, id: &str) -> AvmResult<()> {
        self.client.boot(id)?;
        self.refresh()?;
        Ok(())
    }

    /// 停止实例并刷新视图。
    pub fn stop(&mut self, id: &str) -> AvmResult<()> {
        self.client.stop(id)?;
        self.refresh()?;
        Ok(())
    }

    /// 删除实例并刷新视图。
    pub fn delete(&mut self, id: &str) -> AvmResult<()> {
        self.client.delete(id)?;
        self.refresh()?;
        Ok(())
    }

    /// 处理托盘动作（NSH 事件回调的数据路径）。
    pub fn on_tray_action(&mut self, action: TrayAction) -> AvmResult<String> {
        match action {
            TrayAction::Refresh => {
                let view = self.refresh()?;
                Ok(view.render())
            }
            TrayAction::ShowWindow => Ok("window".into()),
            TrayAction::Quit => Ok("quit".into()),
        }
    }
}

/// M7：NSH 托盘注册存根（Windows 真机阶段接 feature `nsh`）。
pub fn setup_tray(_menu: &TrayMenu) -> AvmResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 起 daemon + 全链路面板操作（multi_thread：client 阻塞调用）。
    #[tokio::test(flavor = "multi_thread")]
    async fn panel_manages_instances_e2e() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let handle = tokio::spawn(async move { avm_daemon::serve(addr).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut panel = Panel::new(format!("http://{addr}"));
        // 托盘菜单模型
        let menu = TrayMenu::default_menu();
        assert_eq!(menu.labels(), vec!["显示面板", "刷新", "退出"]);

        // create（面板动作）
        panel.create("p1", 2, 1024).unwrap();
        let view = panel.view().unwrap();
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.rows[0].state, "Created");

        // 渲染表格包含 ID 与状态
        let text = view.render();
        assert!(text.contains("p1"));
        assert!(text.contains("Created"));

        // boot → Booting
        panel.boot("p1").unwrap();
        assert_eq!(panel.view().unwrap().rows[0].state, "Booting");

        // stop → Created
        panel.stop("p1").unwrap();
        assert_eq!(panel.view().unwrap().rows[0].state, "Created");

        // 托盘 Refresh 动作返回渲染文本
        let out = panel.on_tray_action(TrayAction::Refresh).unwrap();
        assert!(out.contains("p1"));

        // delete
        panel.delete("p1").unwrap();
        assert!(panel.view().unwrap().rows.is_empty());

        handle.abort();
    }

    #[test]
    fn empty_view_renders_placeholder() {
        let v = PanelView::default();
        assert_eq!(v.render(), "（无实例）\n");
    }

    #[test]
    fn tray_action_show_and_quit() {
        // 不连 daemon：ShowWindow/Quit 不发请求
        let mut p = Panel::new("http://127.0.0.1:1");
        assert_eq!(p.on_tray_action(TrayAction::ShowWindow).unwrap(), "window");
        assert_eq!(p.on_tray_action(TrayAction::Quit).unwrap(), "quit");
    }
}
