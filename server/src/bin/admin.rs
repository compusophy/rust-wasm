use sqlx::postgres::PgPoolOptions;
use std::env;

#[tokio::main]
async fn main() {
    // Load .env file if it exists
    dotenv::dotenv().ok();

    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    
    println!("Connecting to database...");
    // Debug: Print the host we are trying to connect to (simple parsing)
    if let Some(host_start) = database_url.find('@') {
        let host_part = &database_url[host_start+1..];
        println!("Debug: Trying to connect to host: '{}'", host_part);
    }

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("Failed to connect to DB");

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_help();
        return;
    }

    match args[1].as_str() {
        "set-version" => {
            if args.len() < 3 {
                println!("Error: Missing version argument.");
                print_help();
                return;
            }
            let version = &args[2];
            // Validate it's a number
            if version.parse::<u32>().is_err() {
                println!("Error: Version must be a number.");
                return;
            }

            sqlx::query("INSERT INTO server_config (key, value) VALUES ('min_client_version', $1) ON CONFLICT (key) DO UPDATE SET value = $1")
                .bind(version)
                .execute(&pool)
                .await
                .expect("Failed to update version");
            println!("✅ Success: min_client_version set to {}", version);
        }
        "reset-db" => {
            println!("⚠️  WARNING: This will WIPE ALL PLAYER DATA.");
            println!("Are you sure? Type 'yes' to confirm:");
            
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).unwrap();
            
            if input.trim() == "yes" {
                println!("Resetting database...");
                // Truncate tables in order. CASCADE handles FKs but being explicit is nice.
                // We restart identity to reset IDs to 1.
                if let Err(e) = sqlx::query("TRUNCATE TABLE units, players RESTART IDENTITY CASCADE")
                    .execute(&pool)
                    .await 
                {
                    println!("Error resetting DB: {}", e);
                } else {
                    println!("✅ Success: Database reset complete.");
                }
            } else {
                println!("Operation cancelled.");
            }
        }
        _ => {
            println!("Unknown command: {}", args[1]);
            print_help();
        }
    }
}

fn print_help() {
    println!("Temty Admin Tool");
    println!("Usage:");
    println!("  cargo run --bin admin -- set-version <VERSION>");
    println!("  cargo run --bin admin -- reset-db");
    println!("\nEnvironment Variable DATABASE_URL must be set.");
}
