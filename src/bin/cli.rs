use std::env;
use std::process;

use kv_engine::Engine;

fn usage() {
    eprintln!("Usage: kv-cli <db-path> <command> [args...]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  put <key> <value>   Write a key-value pair");
    eprintln!("  get <key>           Read the value for a key");
    eprintln!("  delete <key>        Delete a key");
    eprintln!("  scan <start> <end>  Scan keys in [start, end) range");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  kv-cli ./mydb put name rust");
    eprintln!("  kv-cli ./mydb get name");
    eprintln!("  kv-cli ./mydb delete name");
    process::exit(1);
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        usage();
    }

    let db_path = &args[1];
    let command = &args[2];

    let mut db = Engine::open(std::path::Path::new(db_path))
        .unwrap_or_else(|e| {
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
            db.put(key, value).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                process::exit(1);
            });
            println!("OK");
        }
        "get" => {
            if args.len() < 4 {
                eprintln!("Usage: kv-cli <db-path> get <key>");
                process::exit(1);
            }
            let key = args[3].as_bytes();
            match db.get(key).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                process::exit(1);
            }) {
                Some(value) => {
                    // Try to display as UTF-8 string, fall back to hex.
                    match std::str::from_utf8(&value) {
                        Ok(s) => println!("{}", s),
                        Err(_) => println!("{}", hex(&value)),
                    }
                }
                None => println!("(nil)"),
            }
        }
        "delete" => {
            if args.len() < 4 {
                eprintln!("Usage: kv-cli <db-path> delete <key>");
                process::exit(1);
            }
            let key = args[3].as_bytes();
            db.delete(key).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                process::exit(1);
            });
            println!("OK");
        }
        "scan" => {
            if args.len() < 5 {
                eprintln!("Usage: kv-cli <db-path> scan <start> <end>");
                process::exit(1);
            }
            // Scan is not yet implemented on Engine — placeholder.
            eprintln!("scan not yet implemented in Phase 1");
            process::exit(1);
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            usage();
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
}
