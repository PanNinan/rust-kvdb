use std::env;

use kv_engine::{DB, Options};

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    let db_path = args.get(1).map(|s| s.as_str()).unwrap_or("./data");
    let addr = args.get(2).map(|s| s.as_str()).unwrap_or("127.0.0.1:8080");

    let opts = Options::default();
    let db = DB::open_with_options(std::path::Path::new(db_path), opts)
        .expect("failed to open database");

    println!("Database opened at: {}", db_path);

    if let Err(e) = kv_engine::http::start(db, addr).await {
        eprintln!("Server error: {}", e);
        std::process::exit(1);
    }
}
