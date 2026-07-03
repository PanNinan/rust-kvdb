use std::env;
use std::process;
use std::time::Instant;

use kv_engine::{DB, Options};

fn usage() {
    eprintln!("Usage: kv-cli <db-path> <command> [args...]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  put <key> <value>         Write a key-value pair");
    eprintln!("  get <key>                 Read the value for a key");
    eprintln!("  delete <key>              Delete a key");
    eprintln!("  scan <start> <end>        Scan keys in [start, end) range");
    eprintln!("  batch put k1 v1 put k2 v2 ...  Atomic batch write");
    eprintln!("  stats                     Show database metrics");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  kv-cli ./mydb put name rust");
    eprintln!("  kv-cli ./mydb get name");
    eprintln!("  kv-cli ./mydb scan a z");
    process::exit(1);
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        usage();
    }

    let db_path = &args[1];
    let command = &args[2];

    let opts = Options::default();
    let db = DB::open_with_options(std::path::Path::new(db_path), opts).unwrap_or_else(|e| {
        eprintln!("Error opening database: {}", e);
        process::exit(1);
    });

    match command.as_str() {
        "put" => {
            if args.len() < 5 {
                eprintln!("Usage: kv-cli <db-path> put <key> <value>");
                process::exit(1);
            }
            let key = args[3].as_bytes();
            let value = args[4].as_bytes();
            let start = Instant::now();
            db.put(key, value).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                process::exit(1);
            });
            println!("OK ({:.3?})", start.elapsed());
        }
        "get" => {
            if args.len() < 4 {
                eprintln!("Usage: kv-cli <db-path> get <key>");
                process::exit(1);
            }
            let key = args[3].as_bytes();
            let start = Instant::now();
            match db.get(key).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                process::exit(1);
            }) {
                Some(value) => match std::str::from_utf8(&value) {
                    Ok(s) => println!("{} ({:.3?})", s, start.elapsed()),
                    Err(_) => println!(
                        "{} ({:.3?})",
                        hex(&value),
                        start.elapsed()
                    ),
                },
                None => println!("(nil) ({:.3?})", start.elapsed()),
            }
        }
        "delete" => {
            if args.len() < 4 {
                eprintln!("Usage: kv-cli <db-path> delete <key>");
                process::exit(1);
            }
            let key = args[3].as_bytes();
            let start = Instant::now();
            db.delete(key).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                process::exit(1);
            });
            println!("OK ({:.3?})", start.elapsed());
        }
        "scan" => {
            if args.len() < 5 {
                eprintln!("Usage: kv-cli <db-path> scan <start> <end>");
                process::exit(1);
            }
            let start_key = args[3].as_bytes();
            let end_key = args[4].as_bytes();
            let timer = Instant::now();
            let results = db.scan(start_key, end_key).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                process::exit(1);
            });
            for (k, v) in &results {
                let key_str = String::from_utf8_lossy(k);
                let val_str = String::from_utf8_lossy(v);
                println!("{} = {}", key_str, val_str);
            }
            eprintln!("({} results, {:.3?})", results.len(), timer.elapsed());
        }
        "stats" => {
            let m = db.metrics();
            println!("Writes:       {}", m.writes);
            println!("Reads:        {}", m.reads);
            println!("Deletes:      {}", m.deletes);
            println!("Compactions:  {}", m.compactions);
            println!("Flushes:      {}", m.flushes);
        }
        "batch" => {
            let mut batch = kv_engine::WriteBatch::new();
            let mut i = 3;
            while i + 2 < args.len() {
                match args[i].as_str() {
                    "put" => {
                        batch.put(args[i + 1].as_bytes().to_vec(), args[i + 2].as_bytes().to_vec());
                        i += 3;
                    }
                    "delete" => {
                        batch.delete(args[i + 1].as_bytes().to_vec());
                        i += 2;
                    }
                    other => {
                        eprintln!("Unknown batch op: {}", other);
                        process::exit(1);
                    }
                }
            }
            let start = Instant::now();
            db.write_batch(&batch).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                process::exit(1);
            });
            println!("OK: {} ops ({:.3?})", batch.len(), start.elapsed());
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            usage();
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ")
}
