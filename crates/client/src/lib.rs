//! avm-client：CLI 与 NSH 面板共用的 daemon 客户端。

use avm_core::{AvmError, AvmResult, InstanceState};

/// 连接 avmd 的 HTTP 客户端（M6 引入真实 HTTP；骨架为接口面）。
#[derive(Debug, Clone)]
pub struct DaemonClient {
    pub base_url: String,
}

impl DaemonClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    pub fn list_instances(&self) -> AvmResult<Vec<String>> {
        Err(AvmError::Ipc("client: M6 实现真实 HTTP 调用".into()))
    }

    pub fn get_state(&self, id: &str) -> AvmResult<InstanceState> {
        Err(AvmError::Ipc(format!("client: 查询 {id} 状态，M6 实现")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builds() {
        let c = DaemonClient::new("http://127.0.0.1:8080");
        assert_eq!(c.base_url, "http://127.0.0.1:8080");
    }
}
