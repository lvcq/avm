//! avm-daemon：avmd 后台守护（M6）——持有 VmStore 并对外提供 HTTP API。

use std::net::SocketAddr;

use avm_api::VmStore;
use avm_core::AvmResult;

/// 启动 daemon HTTP 服务（阻塞当前线程；内部自建 tokio runtime）。
pub fn run_daemon(listen: SocketAddr) -> AvmResult<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| avm_core::AvmError::Ipc(format!("tokio runtime 构建失败：{e}")))?;
    rt.block_on(serve(listen))
}

/// 在当前 tokio runtime 中启动服务（测试/嵌入用）。
pub async fn serve(listen: SocketAddr) -> AvmResult<()> {
    let store = VmStore::shared();
    let app = avm_api::router(store);
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| avm_core::AvmError::Ipc(format!("监听 {listen} 失败：{e}")))?;
    axum::serve(listener, app)
        .await
        .map_err(|e| avm_core::AvmError::Ipc(format!("HTTP 服务异常：{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn serve_binds_and_health_ok() {
        // 绑定随机空闲端口
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        // 直接测 serve 的绑定路径（用已占用端口应报错）
        let r = serve(addr).await;
        assert!(r.is_err(), "端口被占应绑定失败");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_daemon_serves_requests() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener); // 释放端口给 serve
        let handle = tokio::spawn(async move { serve(addr).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        // 用 client crate 打 health
        let client = avm_client::Client::new(&format!("http://{addr}"));
        let status = client.health().unwrap();
        assert_eq!(status, "ok");
        handle.abort();
    }
}

/// 轻量实例注册表（pool 骨架依赖；与 VmStore 并存，M8 后由 pool 直接持有）。
#[derive(Debug, Default, Clone)]
pub struct InstanceRegistry {
    inner: std::sync::Arc<
        std::sync::Mutex<std::collections::BTreeMap<String, avm_core::InstanceState>>,
    >,
}

impl InstanceRegistry {
    pub fn register(
        &self,
        cfg: &avm_core::InstanceConfig,
        state: avm_core::InstanceState,
    ) -> avm_core::AvmResult<()> {
        let mut map = self
            .inner
            .lock()
            .map_err(|_| avm_core::AvmError::Ipc("registry 锁中毒".into()))?;
        if map.contains_key(&cfg.id) {
            return Err(avm_core::AvmError::Config(format!(
                "实例 {} 已注册",
                cfg.id
            )));
        }
        map.insert(cfg.id.clone(), state);
        Ok(())
    }

    pub fn state(&self, id: &str) -> Option<avm_core::InstanceState> {
        self.inner.lock().ok()?.get(id).copied()
    }

    pub fn list(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn set_state(&self, id: &str, state: avm_core::InstanceState) -> avm_core::AvmResult<()> {
        let mut map = self
            .inner
            .lock()
            .map_err(|_| avm_core::AvmError::Ipc("registry 锁中毒".into()))?;
        match map.get_mut(id) {
            Some(s) => {
                *s = state;
                Ok(())
            }
            None => Err(avm_core::AvmError::Config(format!("未知实例 {id}"))),
        }
    }
}
