//! avm-api：daemon 的 HTTP 路由与 DTO（M6）。
//!
//! 端点：
//! - GET    /health          存活探测
//! - GET    /vms             实例列表
//! - POST   /vms             创建实例（校验 vcpus/memory）
//! - GET    /vms/{id}         实例详情
//! - DELETE /vms/{id}         删除实例
//! - POST   /vms/{id}/boot    Created → Booting
//! - POST   /vms/{id}/stop    Booting|Running → Created
//!
//! M6 状态机为 daemon 内存态；真实分区/引导接线在 daemon 层（WHPX 真机阶段收口）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

/// 实例状态（HTTP 可见字符串）。
pub const ST_CREATED: &str = "Created";
pub const ST_BOOTING: &str = "Booting";
pub const ST_RUNNING: &str = "Running";

/// daemon 内存实例记录。
#[derive(Debug, Clone, Serialize)]
pub struct StoredVm {
    pub id: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub state: String,
}

/// 创建请求体。
#[derive(Debug, Deserialize)]
pub struct CreateVmReq {
    pub id: String,
    pub vcpus: u32,
    pub memory_mib: u64,
}

/// 共享状态。
#[derive(Default)]
pub struct VmStore {
    vms: Mutex<BTreeMap<String, StoredVm>>,
}

impl VmStore {
    pub fn shared() -> Arc<VmStore> {
        Arc::new(Self::default())
    }

    fn insert(&self, vm: StoredVm) -> Result<(), (StatusCode, String)> {
        let mut vms = self
            .vms
            .lock()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "锁中毒".into()))?;
        if vms.contains_key(&vm.id) {
            return Err((StatusCode::CONFLICT, format!("实例 {} 已存在", vm.id)));
        }
        vms.insert(vm.id.clone(), vm);
        Ok(())
    }

    fn list(&self) -> Vec<StoredVm> {
        self.vms
            .lock()
            .map(|v| v.values().cloned().collect())
            .unwrap_or_default()
    }

    fn get(&self, id: &str) -> Option<StoredVm> {
        self.vms.lock().ok()?.get(id).cloned()
    }

    fn remove(&self, id: &str) -> Option<StoredVm> {
        self.vms.lock().ok()?.remove(id)
    }

    fn set_state(
        &self,
        id: &str,
        from: &[&str],
        to: &str,
    ) -> Result<StoredVm, (StatusCode, String)> {
        let mut vms = self
            .vms
            .lock()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "锁中毒".into()))?;
        let vm = vms
            .get_mut(id)
            .ok_or_else(|| (StatusCode::NOT_FOUND, format!("实例 {id} 不存在")))?;
        if !from.contains(&vm.state.as_str()) {
            return Err((
                StatusCode::CONFLICT,
                format!("状态 {} 不允许迁移到 {to}", vm.state),
            ));
        }
        vm.state = to.to_string();
        Ok(vm.clone())
    }
}

/// 校验创建请求。
fn validate(req: &CreateVmReq) -> Result<(), (StatusCode, String)> {
    if req.id.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "id 不能为空".into()));
    }
    if req.vcpus == 0 || req.vcpus > 32 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("vcpus={} 超出 [1,32]", req.vcpus),
        ));
    }
    if req.memory_mib < 512 {
        return Err((StatusCode::BAD_REQUEST, "memory_mib 至少 512".into()));
    }
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn list_vms(State(store): State<Arc<VmStore>>) -> Json<Vec<StoredVm>> {
    Json(store.list())
}

async fn create_vm(
    State(store): State<Arc<VmStore>>,
    Json(req): Json<CreateVmReq>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    validate(&req)?;
    let vm = StoredVm {
        id: req.id,
        vcpus: req.vcpus,
        memory_mib: req.memory_mib,
        state: ST_CREATED.to_string(),
    };
    store.insert(vm.clone())?;
    Ok((StatusCode::CREATED, Json(vm)))
}

async fn get_vm(
    State(store): State<Arc<VmStore>>,
    Path(id): Path<String>,
) -> Result<Json<StoredVm>, (StatusCode, String)> {
    store
        .get(&id)
        .map(Json)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("实例 {id} 不存在")))
}

async fn delete_vm(
    State(store): State<Arc<VmStore>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    store
        .remove(&id)
        .map(|_| StatusCode::NO_CONTENT)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("实例 {id} 不存在")))
}

async fn boot_vm(
    State(store): State<Arc<VmStore>>,
    Path(id): Path<String>,
) -> Result<Json<StoredVm>, (StatusCode, String)> {
    store.set_state(&id, &[ST_CREATED], ST_BOOTING).map(Json)
}

async fn stop_vm(
    State(store): State<Arc<VmStore>>,
    Path(id): Path<String>,
) -> Result<Json<StoredVm>, (StatusCode, String)> {
    store
        .set_state(&id, &[ST_BOOTING, ST_RUNNING], ST_CREATED)
        .map(Json)
}

/// 构建路由（daemon 与测试共用）。
pub fn router(store: Arc<VmStore>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/vms", get(list_vms).post(create_vm))
        .route("/vms/{id}", get(get_vm).delete(delete_vm))
        .route("/vms/{id}/boot", post(boot_vm))
        .route("/vms/{id}/stop", post(stop_vm))
        .with_state(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn req(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<String>,
    ) -> (StatusCode, serde_json::Value) {
        let builder = axum::http::Request::builder().method(method).uri(uri);
        let request = match body {
            Some(b) => builder
                .header("content-type", "application/json")
                .body(Body::from(b))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let resp = app.oneshot(request).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    fn app() -> Router {
        router(VmStore::shared())
    }

    #[tokio::test]
    async fn health_ok() {
        let (st, v) = req(app(), "GET", "/health", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["status"], "ok");
    }

    #[tokio::test]
    async fn create_list_get_delete_lifecycle() {
        let app = app();
        let body = serde_json::json!({"id":"vm1","vcpus":2,"memory_mib":1024}).to_string();
        let (st, v) = req(app.clone(), "POST", "/vms", Some(body)).await;
        assert_eq!(st, StatusCode::CREATED);
        assert_eq!(v["state"], ST_CREATED);

        let (st, v) = req(app.clone(), "GET", "/vms", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v.as_array().unwrap().len(), 1);

        let (st, v) = req(app.clone(), "GET", "/vms/vm1", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["id"], "vm1");

        // boot: Created → Booting
        let (st, v) = req(app.clone(), "POST", "/vms/vm1/boot", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], ST_BOOTING);

        // stop: Booting → Created
        let (st, v) = req(app.clone(), "POST", "/vms/vm1/stop", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], ST_CREATED);

        let (st, _) = req(app.clone(), "DELETE", "/vms/vm1", None).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let (st, _) = req(app.clone(), "GET", "/vms/vm1", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn duplicate_create_conflicts() {
        let app = app();
        let body = serde_json::json!({"id":"vm1","vcpus":2,"memory_mib":1024}).to_string();
        req(app.clone(), "POST", "/vms", Some(body.clone())).await;
        let (st, _) = req(app.clone(), "POST", "/vms", Some(body)).await;
        assert_eq!(st, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn invalid_create_rejected() {
        let app = app();
        let bad = serde_json::json!({"id":"vm1","vcpus":0,"memory_mib":64}).to_string();
        let (st, _) = req(app.clone(), "POST", "/vms", Some(bad)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn boot_requires_created_state() {
        let app = app();
        let body = serde_json::json!({"id":"vm2","vcpus":1,"memory_mib":512}).to_string();
        req(app.clone(), "POST", "/vms", Some(body)).await;
        req(app.clone(), "POST", "/vms/vm2/boot", None).await;
        // 再次 boot → 409
        let (st, _) = req(app.clone(), "POST", "/vms/vm2/boot", None).await;
        assert_eq!(st, StatusCode::CONFLICT);
        // stop 两次：第二次已 Created → 409
        req(app.clone(), "POST", "/vms/vm2/stop", None).await;
        let (st, _) = req(app.clone(), "POST", "/vms/vm2/stop", None).await;
        assert_eq!(st, StatusCode::CONFLICT);
    }
}
