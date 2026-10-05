//! meshsexcel-node:局域网协同表格节点。
//!
//! 单二进制 = REST API + 内嵌 Web UI + libp2p(mDNS/gossipsub)+ SQLite 存档。
//! 同一局域网跑两个实例即自动互发现并同步 block。

use clap::Parser;
use meshsexcel_node::{api, net, state};
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(
    name = "meshsexcel-node",
    version,
    about = "MeshSExcel 局域网协同表格节点"
)]
struct Args {
    /// 节点显示名(也是 block 里的 author)
    #[arg(long, default_value = "node-1")]
    node_id: String,

    /// 数据目录(存档 + 节点身份)
    #[arg(long, default_value = "./data")]
    db_dir: String,

    /// REST/Web UI 端口
    #[arg(long, default_value_t = 8443)]
    http_port: u16,

    /// libp2p 监听端口
    #[arg(long, default_value_t = 20001)]
    p2p_port: u16,

    /// gossip 主题(同一局域网内一致才能互通)
    #[arg(long, default_value = "meshsexcel-demo")]
    topic: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // 发布通道:AppState 持发送端,网络任务持接收端
    let (publish_tx, publish_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    let app = Arc::new(state::AppState::boot(
        std::path::Path::new(&args.db_dir),
        &args.node_id,
        publish_tx,
    )?);

    let _net_task = net::spawn(
        &args.node_id,
        args.p2p_port,
        &args.topic,
        app.clone(),
        publish_rx,
    )?;

    let addr = format!("0.0.0.0:{}", args.http_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!(
        "[{}] HTTP/UI  http://localhost:{}",
        args.node_id, args.http_port
    );
    println!(
        "[{}] p2p      tcp/{}, topic \"{}\"",
        args.node_id, args.p2p_port, args.topic
    );
    println!("[{}] data     {}", args.node_id, args.db_dir);

    axum::serve(listener, api::router(app)).await?;
    Ok(())
}
