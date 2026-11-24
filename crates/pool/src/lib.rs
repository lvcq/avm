//! avm-pool：实例池 / 快照管理。
//!
//! 目标：并发维持 N 台 Android 实例，供 Airtest / Appium / uiautomator2
//! 外部自动化工具按需租借。快照用于快速恢复到干净状态。

use avm_core::{AvmError, AvmResult, InstanceConfig, InstanceState};
use avm_daemon::InstanceRegistry;
use std::collections::VecDeque;

/// 简单实例池骨架：空闲队列 + 租借/归还语义。
pub struct InstancePool {
    registry: InstanceRegistry,
    free: VecDeque<String>,
    capacity: usize,
}

impl InstancePool {
    pub fn new(capacity: usize) -> Self {
        Self {
            registry: InstanceRegistry::default(),
            free: VecDeque::new(),
            capacity,
        }
    }

    pub fn add(&mut self, cfg: InstanceConfig) -> AvmResult<()> {
        if self.registry.list().len() >= self.capacity {
            return Err(AvmError::Config(format!(
                "实例池容量已满（{}/{}）",
                self.registry.list().len(),
                self.capacity
            )));
        }
        self.registry.register(&cfg, InstanceState::Created)?;
        self.free.push_back(cfg.id);
        Ok(())
    }

    /// 租借一台空闲实例。
    pub fn acquire(&mut self) -> Option<String> {
        self.free.pop_front()
    }

    /// 归还实例（M8 后：先恢复快照再回池）。
    pub fn release(&mut self, id: String) -> AvmResult<()> {
        if self.registry.state(&id).is_none() {
            return Err(AvmError::Config(format!("未知实例 {id}")));
        }
        self.free.push_back(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(id: &str) -> InstanceConfig {
        InstanceConfig {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn acquire_release_roundtrip() {
        let mut p = InstancePool::new(2);
        assert!(p.add(cfg("vm0")).is_ok());
        assert!(p.add(cfg("vm1")).is_ok());
        assert!(p.add(cfg("vm2")).is_err()); // 容量满
        let got = p.acquire();
        assert_eq!(got.as_deref(), Some("vm0"));
        assert!(p.release(got.unwrap()).is_ok());
    }

    #[test]
    fn release_unknown_errors() {
        let mut p = InstancePool::new(1);
        assert!(p.release("ghost".into()).is_err());
    }
}
