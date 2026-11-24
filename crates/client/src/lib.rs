//! avm-client：daemon HTTP 客户端（CLI 与 M7 面板共用）。

use avm_core::{AvmError, AvmResult};
use serde::Deserialize;

/// daemon 客户端（阻塞 HTTP，ureq）。
#[derive(Debug)]
pub struct Client {
    base: String,
    /// ureq agent（Debug 直通）。
    agent: ureq::Agent,
}

/// 实例 DTO（与 avm-api::StoredVm 字段一致）。
#[derive(Debug, Clone, Deserialize)]
pub struct VmDto {
    pub id: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub state: String,
}

impl Client {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    fn map_err(e: ureq::Error) -> AvmError {
        AvmError::Ipc(format!("daemon 请求失败：{e}"))
    }

    /// GET /health → status 字符串。
    pub fn health(&self) -> AvmResult<String> {
        let r: serde_json::Value = self
            .agent
            .get(&format!("{}/health", self.base))
            .call()
            .map_err(Self::map_err)?
            .into_json()
            .map_err(|e| AvmError::Ipc(format!("health 响应解析失败：{e}")))?;
        Ok(r["status"].as_str().unwrap_or_default().to_string())
    }

    /// GET /vms → 实例列表。
    pub fn list(&self) -> AvmResult<Vec<VmDto>> {
        let r: Vec<VmDto> = self
            .agent
            .get(&format!("{}/vms", self.base))
            .call()
            .map_err(Self::map_err)?
            .into_json()
            .map_err(|e| AvmError::Ipc(format!("list 响应解析失败：{e}")))?;
        Ok(r)
    }

    /// POST /vms → 创建实例。
    pub fn create(&self, id: &str, vcpus: u32, memory_mib: u64) -> AvmResult<VmDto> {
        let body = serde_json::json!({"id": id, "vcpus": vcpus, "memory_mib": memory_mib});
        let r: VmDto = self
            .agent
            .post(&format!("{}/vms", self.base))
            .send_json(body)
            .map_err(Self::map_err)?
            .into_json()
            .map_err(|e| AvmError::Ipc(format!("create 响应解析失败：{e}")))?;
        Ok(r)
    }

    /// GET /vms/:id → 实例详情。
    pub fn get(&self, id: &str) -> AvmResult<Option<VmDto>> {
        match self.agent.get(&format!("{}/vms/{id}", self.base)).call() {
            Ok(resp) => resp
                .into_json()
                .map(Some)
                .map_err(|e| AvmError::Ipc(format!("get 响应解析失败：{e}"))),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(Self::map_err(e)),
        }
    }

    /// POST /vms/:id/boot。
    pub fn boot(&self, id: &str) -> AvmResult<VmDto> {
        let r: VmDto = self
            .agent
            .post(&format!("{}/vms/{id}/boot", self.base))
            .call()
            .map_err(Self::map_err)?
            .into_json()
            .map_err(|e| AvmError::Ipc(format!("boot 响应解析失败：{e}")))?;
        Ok(r)
    }

    /// POST /vms/:id/stop。
    pub fn stop(&self, id: &str) -> AvmResult<VmDto> {
        let r: VmDto = self
            .agent
            .post(&format!("{}/vms/{id}/stop", self.base))
            .call()
            .map_err(Self::map_err)?
            .into_json()
            .map_err(|e| AvmError::Ipc(format!("stop 响应解析失败：{e}")))?;
        Ok(r)
    }

    /// DELETE /vms/:id。
    pub fn delete(&self, id: &str) -> AvmResult<bool> {
        match self.agent.delete(&format!("{}/vms/{id}", self.base)).call() {
            Ok(_) => Ok(true),
            Err(ureq::Error::Status(404, _)) => Ok(false),
            Err(e) => Err(Self::map_err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无 daemon 时的错误路径（不 panic、返回 AvmError）。
    #[test]
    fn unreachable_daemon_maps_error() {
        let c = Client::new("http://127.0.0.1:1");
        assert!(c.health().is_err());
    }
}
