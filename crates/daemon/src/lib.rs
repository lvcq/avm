//! avm-daemon：`avmd` 后台守护进程。
//!
//! 持有全部 VM 实例与 HTTP API（avm-api）；NSH 面板与 CLI 均为其客户端。
//! M6 实现：tokio 运行时 + 实例注册表 + 优雅退出。

use avm_core::{AvmResult, InstanceConfig, InstanceState};
use std::collections::HashMap;

/// 实例注册表：daemon 的核心状态。
#[derive(Debug, Default)]
pub struct InstanceRegistry {
    instances: HashMap<String, InstanceState>,
}

impl InstanceRegistry {
    pub fn register(&mut self, cfg: &InstanceConfig, state: InstanceState) -> AvmResult<()> {
        if self.instances.contains_key(&cfg.id) {
            return Err(avm_core::AvmError::Config(format!(
                "实例 {} 已存在",
                cfg.id
            )));
        }
        self.instances.insert(cfg.id.clone(), state);
        Ok(())
    }

    pub fn state(&self, id: &str) -> Option<InstanceState> {
        self.instances.get(id).copied()
    }

    pub fn list(&self) -> Vec<(String, InstanceState)> {
        let mut v: Vec<_> = self
            .instances
            .iter()
            .map(|(k, s)| (k.clone(), *s))
            .collect();
        v.sort();
        v
    }
}

/// daemon 启动入口（M6：绑定 HTTP 监听并常驻）。
pub fn run_daemon(_listen: &str) -> AvmResult<()> {
    // M6: tokio + axum(avm-api::router) 在此展开
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_list() {
        let mut r = InstanceRegistry::default();
        let cfg = InstanceConfig {
            id: "vm0".into(),
            ..Default::default()
        };
        assert!(r.register(&cfg, InstanceState::Running).is_ok());
        assert!(r.register(&cfg, InstanceState::Running).is_err());
        assert_eq!(r.state("vm0"), Some(InstanceState::Running));
        assert_eq!(r.list().len(), 1);
    }
}
