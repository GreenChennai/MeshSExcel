//! REST API 集成测试:在进程内起完整 Router(tower oneshot),
//! 覆盖 openapi 端点与扩展端点的主干路径。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use meshsexcel_node::api;
use meshsexcel_node::state::AppState;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt; // oneshot

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "meshsexcel-node-test-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_app(tag: &str) -> (axum::Router, PathBuf) {
    let dir = temp_dir(tag);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(AppState::boot(&dir, "test-node", tx).unwrap());
    (api::router(state), dir)
}

async fn json_response(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, v)
}

#[tokio::test]
async fn create_document_and_grid_flow() {
    let (app, dir) = test_app("flow");
    let _ = &dir;

    // 建文档
    let (status, doc) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/documents")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"name": "测试台账", "owner": "alice"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "建文档应 201: {doc}");
    let doc_id = doc["id"].as_str().unwrap().to_string();
    assert!(doc_id.starts_with("doc-"));
    assert_eq!(doc["name"], "测试台账");

    // 文档列表
    let (status, list) = json_response(
        &app,
        Request::builder()
            .uri("/v1/documents")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    // 写入:值 + 公式
    let ops = json!([
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"Sheet1","row":1,"col":1,"value":"12","formula":null,"style":null},
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"Sheet1","row":2,"col":1,"value":"30","formula":null,"style":null},
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"Sheet1","row":3,"col":1,"value":null,"formula":"=SUM(A1:A2)","style":null}
    ]);
    let (status, block) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/ops"))
            .header("content-type", "application/json")
            .body(Body::from(ops.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "提交 ops 应 201: {block}");
    assert_eq!(block["header"]["author"], "test-node");

    // 读取网格:公式应已计算
    let (status, grid) = json_response(
        &app,
        Request::builder()
            .uri(format!("/v1/documents/{doc_id}/cells"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(grid["name"], "测试台账");
    let cells = grid["sheets"][0]["cells"].as_array().unwrap();
    let a3 = cells
        .iter()
        .find(|c| c["row"] == 3 && c["col"] == 1)
        .expect("A3 应存在");
    assert_eq!(a3["value"], "42", "SUM(A1:A2) = 12+30 = 42");
    assert_eq!(a3["formula"], "=SUM(A1:A2)");

    // block 链:两个 block(建文档 + ops),全部可验签
    let (status, blocks) = json_response(
        &app,
        Request::builder()
            .uri(format!("/v1/documents/{doc_id}/blocks"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let blocks = blocks.as_array().unwrap();
    assert_eq!(blocks.len(), 2, "建文档 + 一次 ops = 2 个 block");
    for b in blocks {
        assert_eq!(b["header"]["doc_id"], doc_id);
    }

    // blocks?since=未知 hash → 返回全部(已知 hash 的祖先裁剪逻辑在 core 单测覆盖)
    let since_uri = format!("/v1/documents/{doc_id}/blocks?since=deadbeef");
    let (status, all) = json_response(
        &app,
        Request::builder()
            .uri(since_uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all.as_array().unwrap().len(), 2, "未知 since 返回全部");

    // 404
    let (status, _) = json_response(
        &app,
        Request::builder()
            .uri("/v1/documents/doc-nope/cells")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn export_xlsx_and_csv() {
    let (app, _dir) = test_app("export");
    let (_, doc) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/documents")
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "导出"}).to_string()))
            .unwrap(),
    )
    .await;
    let doc_id = doc["id"].as_str().unwrap();

    let ops = json!([
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"Sheet1","row":1,"col":1,"value":"品名","formula":null,"style":null},
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"Sheet1","row":1,"col":2,"value":"数量","formula":null,"style":null},
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"Sheet1","row":2,"col":1,"value":"苹果","formula":null,"style":null},
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"Sheet1","row":2,"col":2,"value":"5","formula":null,"style":null}
    ]);
    let (status, _) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/ops"))
            .header("content-type", "application/json")
            .body(Body::from(ops.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // XLSX:zip 容器
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/documents/{doc_id}/export/xlsx"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(&bytes[..2], b"PK", "xlsx 应是 zip 容器");

    // CSV
    let res = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/documents/{doc_id}/export/csv"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let csv = String::from_utf8(body.to_vec()).unwrap();
    assert!(csv.contains("品名"), "csv 应含表头: {csv}");
    assert!(csv.contains("苹果,5"), "csv 应含数据行: {csv}");
}

#[tokio::test]
async fn csv_import_and_snapshot_restore() {
    let (app, _dir) = test_app("import");
    let (_, doc) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/documents")
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "导入"}).to_string()))
            .unwrap(),
    )
    .await;
    let doc_id = doc["id"].as_str().unwrap().to_string();

    // 导入 CSV
    let csv = "城市,人口\n上海,2487\n北京,2189\n";
    let (status, result) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/import/csv?sheet=人口"))
            .header("content-type", "text/csv")
            .body(Body::from(csv.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "导入应成功: {result}");
    assert_eq!(result["imported_cells"], 6);

    let (_, grid) = json_response(
        &app,
        Request::builder()
            .uri(format!("/v1/documents/{doc_id}/cells"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        grid["sheets"].as_array().unwrap().len(),
        2,
        "Sheet1 + 导入表"
    );
    assert_eq!(grid["sheets"][1]["name"], "人口");

    // 快照:建立 → 再改 → 恢复
    let (status, snap) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/snapshots"))
            .header("content-type", "application/json")
            .body(Body::from(json!({"description": "导入后"}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let snap_id = snap["snapshot_id"].as_str().unwrap().to_string();

    // 恢复前先改一个格子 + 加一格
    let ops = json!([
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"Sheet1","row":1,"col":1,"value":"被覆盖","formula":null,"style":null}
    ]);
    let _ = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/ops"))
            .header("content-type", "application/json")
            .body(Body::from(ops.to_string()))
            .unwrap(),
    )
    .await;

    let (status, restored) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/restore"))
            .header("content-type", "application/json")
            .body(Body::from(json!({"snapshot_id": snap_id}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "恢复应成功: {restored}");

    // 恢复后 Sheet1 的 A1 应回到空(快照时点 Sheet1 无内容)
    let (_, grid) = json_response(
        &app,
        Request::builder()
            .uri(format!("/v1/documents/{doc_id}/cells"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let sheet1 = grid["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Sheet1")
        .expect("Sheet1 应存在");
    assert_eq!(
        sheet1["cells"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "恢复后 Sheet1 应清空回快照状态"
    );
    // 人口表仍在
    assert!(grid["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "人口"));
}

#[tokio::test]
async fn reject_tampered_block() {
    let (app, _dir) = test_app("tamper");
    let (_, doc) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/documents")
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "防篡改"}).to_string()))
            .unwrap(),
    )
    .await;
    let doc_id = doc["id"].as_str().unwrap();

    // 取当前 block 链,篡改载荷后回提交
    let (_, blocks) = json_response(
        &app,
        Request::builder()
            .uri(format!("/v1/documents/{doc_id}/blocks"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let mut forged = blocks[0].clone();
    forged["payload"]["operations"] = json!(["injected-by-attacker"]);

    let (status, err) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/blocks"))
            .header("content-type", "application/json")
            .body(Body::from(forged.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "篡改 block 必须被拒: {err}"
    );
    assert!(err["error"]
        .as_str()
        .unwrap_or("")
        .contains("verification failed"));
}

#[tokio::test]
async fn sheet_add_remove_via_ops() {
    let (app, _dir) = test_app("sheets");
    let (_, doc) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/documents")
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "多表"}).to_string()))
            .unwrap(),
    )
    .await;
    let doc_id = doc["id"].as_str().unwrap().to_string();

    let ops = json!([
        {"stamp":{"ts":0,"author":""},"type":"add_sheet","sheet":"汇总","position":null},
        {"stamp":{"ts":0,"author":""},"type":"set_cell","sheet":"汇总","row":1,"col":1,"value":"合计","formula":null,"style":null}
    ]);
    let (status, _) = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/ops"))
            .header("content-type", "application/json")
            .body(Body::from(ops.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, grid) = json_response(
        &app,
        Request::builder()
            .uri(format!("/v1/documents/{doc_id}/cells"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let names: Vec<&str> = grid["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Sheet1", "汇总"]);

    // 删表
    let ops = json!([
        {"stamp":{"ts":0,"author":""},"type":"remove_sheet","sheet":"汇总"}
    ]);
    let _ = json_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/v1/documents/{doc_id}/ops"))
            .header("content-type", "application/json")
            .body(Body::from(ops.to_string()))
            .unwrap(),
    )
    .await;
    let (_, grid) = json_response(
        &app,
        Request::builder()
            .uri(format!("/v1/documents/{doc_id}/cells"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(grid["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["name"] != "汇总"));
}

#[tokio::test]
async fn node_info_and_peers_endpoints() {
    let (app, _dir) = test_app("info");
    let (status, info) = json_response(
        &app,
        Request::builder()
            .uri("/v1/node/info")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["node_id"], "test-node");
    assert!(info["pubkey"].as_str().unwrap().starts_with("ed25519:"));

    let (status, peers) = json_response(
        &app,
        Request::builder()
            .uri("/v1/nodes/peers")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(peers.as_array().unwrap().len(), 0);
}
