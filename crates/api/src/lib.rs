//! avm-api：HTTP 路由与 DTO（M6）。
//!
//! 目标路由（axum）：
//! GET    /api/instances            列出实例
//! POST   /api/instances            创建实例
//! POST   /api/instances/:id/start  启动
//! POST   /api/instances/:id/stop   停止
//! GET    /api/instances/:id/state  状态
//! POST   /api/instances/:id/snapshot  快照/恢复
//! GET    /healthz                  健康检查

use avm_core::InstanceState;
use serde_placeholder::InstanceDto;

/// DTO 定义（M6 引入 serde 后真实化；骨架阶段为纯数据结构）。
mod serde_placeholder {
    use avm_core::InstanceState;

    pub struct InstanceDto {
        pub id: String,
        pub state: InstanceState,
    }
}

/// 构造路由表（M6 返回 axum::Router）。
pub fn describe_routes() -> &'static [&'static str] {
    &[
        "GET /api/instances",
        "POST /api/instances",
        "POST /api/instances/:id/start",
        "POST /api/instances/:id/stop",
        "GET /api/instances/:id/state",
        "POST /api/instances/:id/snapshot",
        "GET /healthz",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_documented() {
        assert!(describe_routes().iter().any(|r| r.contains("healthz")));
    }
}
