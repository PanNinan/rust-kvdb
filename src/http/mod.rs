//! HTTP management interface for the KV engine.

pub mod dashboard;
pub mod handler;

use tokio::net::TcpListener;

use crate::engine::DB;

/// Start the HTTP management server.
pub async fn start(db: DB, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let app = handler::routes(db);

    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    println!("╔══════════════════════════════════════════════╗");
    println!("║         rust-kvdb  Management Console        ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  Dashboard:  http://{}             ║", local_addr);
    println!("║  API Docs:   http://{}/api/health   ║", local_addr);
    println!("╚══════════════════════════════════════════════╝");

    axum::serve(listener, app).await?;
    Ok(())
}
