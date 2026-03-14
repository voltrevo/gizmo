mod db;
mod models;
mod server;
mod ws;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;

fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gizmo")
}

fn default_db_path() -> String {
    default_data_dir().join("gizmo.db").to_string_lossy().into_owned()
}

#[derive(Parser)]
#[command(name = "gizmo", about = "WebSocket message server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server in the foreground
    Run {
        /// Port to listen on
        #[arg(long, default_value_t = 10_421)]
        port: u16,

        /// Bearer token (auto-generated and saved on first run if not provided)
        #[arg(long, env = "GIZMO_TOKEN")]
        token: Option<String>,

        /// SQLite database path
        #[arg(long, default_value_t = default_db_path())]
        db: String,

        /// Max history size in bytes
        #[arg(long, default_value_t = 10 * 1024 * 1024 * 1024)]
        max_history_bytes: u64,
    },

    /// Install and start gizmo as a systemd user service (restarts on reboot)
    Start {
        /// Port to listen on
        #[arg(long, default_value_t = 10_421)]
        port: u16,

        /// Bearer token (auto-generated and saved on first run if not provided)
        #[arg(long, env = "GIZMO_TOKEN")]
        token: Option<String>,

        /// SQLite database path
        #[arg(long, default_value_t = default_db_path())]
        db: String,

        /// Max history size in bytes
        #[arg(long, default_value_t = 10 * 1024 * 1024 * 1024)]
        max_history_bytes: u64,
    },

    /// Stop and uninstall the gizmo systemd user service
    Stop,

    /// Generate a new ed25519 keypair
    Keygen,

    /// Publish a message to the server
    Publish {
        /// Server URL (e.g. ws://localhost:10421)
        #[arg(long, env = "GIZMO_URL", default_value = "ws://localhost:10421")]
        url: String,

        /// Bearer token for authentication
        #[arg(long, env = "GIZMO_TOKEN")]
        token: String,

        /// Hex-encoded ed25519 secret key
        #[arg(long, env = "GIZMO_SECRET_KEY")]
        secret_key: String,

        /// Channel (defaults to "default")
        #[arg(long)]
        channel: Option<String>,

        /// Tags (comma-separated)
        #[arg(long)]
        tags: String,

        /// Message body (JSON string)
        #[arg(long)]
        body: String,

        /// Allow list: comma-separated public keys that can see this message
        #[arg(long)]
        allow: Option<String>,

        /// Disallow list: comma-separated public keys that cannot see this message
        #[arg(long)]
        disallow: Option<String>,
    },

    /// Fetch message history from the server
    History {
        /// Server URL (e.g. http://localhost:10421)
        #[arg(long, env = "GIZMO_URL", default_value = "http://localhost:10421")]
        url: String,

        /// Bearer token for authentication
        #[arg(long, env = "GIZMO_TOKEN")]
        token: String,

        /// Hex-encoded ed25519 public key (for access control filtering)
        #[arg(long, env = "GIZMO_PUBLIC_KEY")]
        public_key: Option<String>,

        /// Channel (defaults to "default")
        #[arg(long)]
        channel: Option<String>,

        /// Return messages after this ID
        #[arg(long)]
        after: Option<i64>,

        /// Return messages before this ID
        #[arg(long)]
        before: Option<i64>,

        /// Page size (default 50, max 200)
        #[arg(long)]
        limit: Option<i64>,

        /// Comma-separated tag filter
        #[arg(long)]
        tags: Option<String>,
    },
}

const SERVICE_NAME: &str = "gizmo";

fn ensure_parent_dir(path: &str) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).expect("failed to create data directory");
        }
    }
}

fn resolve_token(token: Option<String>, db: &str) -> String {
    token.unwrap_or_else(|| {
        let token_path = std::path::Path::new(db)
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("token");
        if let Ok(existing) = std::fs::read_to_string(&token_path) {
            let t = existing.trim().to_string();
            if !t.is_empty() {
                eprintln!("loaded token from {}", token_path.display());
                return t;
            }
        }
        let t = hex::encode(rand::random::<[u8; 32]>());
        std::fs::write(&token_path, &t).expect("failed to write token file");
        eprintln!("generated new token and saved to {}", token_path.display());
        println!("token: {t}");
        t
    })
}

const GIT_VERSION: &str = env!("GIZMO_GIT_VERSION");

async fn run_server(port: u16, token: Option<String>, db: String, max_history_bytes: u64) {
    tracing_subscriber::fmt::init();
    ensure_parent_dir(&db);
    let token = resolve_token(token, &db);
    let state = Arc::new(server::AppState::new(&db, token, max_history_bytes));

    // Insert a startup message so clients can see when the server (re)started.
    let msg = state.db.insert_message(
        "0000000000000000000000000000000000000000000000000000000000000000",
        "default",
        &["system".to_string()],
        &serde_json::json!({ "text": format!("server started ({})", GIT_VERSION) }),
        &None,
        &None,
        "",
    );
    let _ = state.broadcast_tx.send(msg);

    let app = server::router(state.clone());
    let addr = format!("0.0.0.0:{port}");
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

fn systemd_install(port: u16, token: Option<String>, db: String, max_history_bytes: u64) {
    let exe = std::env::current_exe().expect("failed to get current executable path");
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

    ensure_parent_dir(&db);
    let token = resolve_token(token, &db);

    // Resolve db to absolute path so systemd can find it regardless of WorkingDirectory
    let db_path = std::fs::canonicalize(&db).unwrap_or_else(|_| std::path::PathBuf::from(&db));
    let working_dir = db_path
        .parent()
        .unwrap_or(std::path::Path::new("."));
    let working_dir = std::fs::canonicalize(working_dir)
        .unwrap_or_else(|_| working_dir.to_path_buf());

    let unit = format!(
        "\
[Unit]
Description=Gizmo WebSocket message server
After=network.target

[Service]
Type=simple
ExecStart={exe} run --port {port} --token {token} --db {db_path} --max-history-bytes {max_history_bytes}
WorkingDirectory={working_dir}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
",
        exe = exe.display(),
        db_path = db_path.display(),
        working_dir = working_dir.display(),
    );

    let service_dir = dirs::home_dir()
        .expect("cannot determine home directory")
        .join(".config/systemd/user");
    std::fs::create_dir_all(&service_dir).expect("failed to create systemd user directory");

    let service_path = service_dir.join(format!("{SERVICE_NAME}.service"));
    std::fs::write(&service_path, &unit).expect("failed to write service file");
    eprintln!("wrote {}", service_path.display());

    run_systemctl(&["daemon-reload"]);
    run_systemctl(&["enable", &format!("{SERVICE_NAME}.service")]);
    run_systemctl(&["start", &format!("{SERVICE_NAME}.service")]);

    // Enable lingering so the user service survives logout
    let user = std::env::var("USER").unwrap_or_default();
    if !user.is_empty() {
        let status = std::process::Command::new("loginctl")
            .args(["enable-linger", &user])
            .status();
        match status {
            Ok(s) if s.success() => eprintln!("enabled linger for user {user}"),
            _ => eprintln!("warning: failed to enable-linger (service may not survive logout)"),
        }
    }

    eprintln!("gizmo started (port {port})");
    eprintln!("check status: systemctl --user status {SERVICE_NAME}");
    eprintln!("view logs:    journalctl --user -u {SERVICE_NAME} -f");
}

fn systemd_uninstall() {
    run_systemctl(&["stop", &format!("{SERVICE_NAME}.service")]);
    run_systemctl(&["disable", &format!("{SERVICE_NAME}.service")]);

    let service_path = dirs::home_dir()
        .expect("cannot determine home directory")
        .join(format!(".config/systemd/user/{SERVICE_NAME}.service"));

    if service_path.exists() {
        std::fs::remove_file(&service_path).expect("failed to remove service file");
        eprintln!("removed {}", service_path.display());
    }

    run_systemctl(&["daemon-reload"]);
    eprintln!("gizmo stopped and service removed");
}

fn run_systemctl(args: &[&str]) {
    let status = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to run systemctl: {e}"));
    if !status.success() {
        eprintln!("warning: systemctl --user {} exited with {status}", args.join(" "));
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            port,
            token,
            db,
            max_history_bytes,
        } => {
            run_server(port, token, db, max_history_bytes).await;
        }

        Command::Start {
            port,
            token,
            db,
            max_history_bytes,
        } => {
            systemd_install(port, token, db, max_history_bytes);
        }

        Command::Stop => {
            systemd_uninstall();
        }

        Command::Keygen => {
            use ed25519_dalek::SigningKey;

            let signing_key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let verifying_key = signing_key.verifying_key();
            println!("secret_key: {}", hex::encode(signing_key.to_bytes()));
            println!("public_key: {}", hex::encode(verifying_key.to_bytes()));
        }

        Command::Publish {
            url,
            token,
            secret_key,
            channel,
            tags,
            body,
            allow,
            disallow,
        } => {
            use ed25519_dalek::{Signer, SigningKey};
            use futures_util::{SinkExt, StreamExt};
            use tokio_tungstenite::tungstenite;

            let sk_bytes = hex::decode(&secret_key).expect("secret_key must be hex-encoded");
            let sk_array: [u8; 32] = sk_bytes
                .try_into()
                .expect("secret_key must be 32 bytes (64 hex chars)");
            let signing_key = SigningKey::from_bytes(&sk_array);
            let pubkey = hex::encode(signing_key.verifying_key().to_bytes());

            let tags_vec: Vec<String> =
                tags.split(',').map(|s| s.trim().to_string()).collect();

            let body_value: serde_json::Value =
                serde_json::from_str(&body).expect("body must be valid JSON");

            let allow_vec = allow.map(|a| {
                a.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            });
            let disallow_vec = disallow.map(|d| {
                d.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            });

            // Build the incoming message to get canonical payload for signing.
            let incoming = models::IncomingMessage {
                channel: channel.clone(),
                tags: tags_vec.clone(),
                body: body_value.clone(),
                allow: allow_vec.clone(),
                disallow: disallow_vec.clone(),
                signature: String::new(), // placeholder, removed by canonical_payload
                ed25519: None,
            };
            let canonical = ws::canonical_payload(&incoming);
            let signature = signing_key.sign(canonical.as_bytes());
            let sig_hex = hex::encode(signature.to_bytes());

            let ws_url = format!("{url}/ws");
            let request = tungstenite::http::Request::builder()
                .uri(&ws_url)
                .header("authorization", format!("Bearer {token}"))
                .header("x-ed25519-pubkey", &pubkey)
                .header("sec-websocket-key", tungstenite::handshake::client::generate_key())
                .header("sec-websocket-version", "13")
                .header("connection", "Upgrade")
                .header("upgrade", "websocket")
                .header("host", url.trim_start_matches("ws://").trim_start_matches("wss://"))
                .body(())
                .expect("failed to build request");

            let (ws_stream, _) = tokio_tungstenite::connect_async(request)
                .await
                .expect("failed to connect to WebSocket");

            let (mut write, mut read) = ws_stream.split();

            let mut msg = serde_json::json!({
                "type": "publish",
                "tags": tags_vec,
                "body": body_value,
                "allow": allow_vec,
                "disallow": disallow_vec,
                "signature": sig_hex,
            });
            if let Some(ref ch) = channel {
                msg["channel"] = serde_json::json!(ch);
            }

            write
                .send(tungstenite::Message::Text(msg.to_string().into()))
                .await
                .expect("failed to send message");

            if let Some(Ok(resp)) = read.next().await {
                println!("{}", resp);
            }

            write.close().await.ok();
        }

        Command::History {
            url,
            token,
            public_key,
            channel,
            after,
            before,
            limit,
            tags,
        } => {
            let client = reqwest::Client::new();
            let mut req = client
                .get(format!("{url}/history"))
                .header("authorization", format!("Bearer {token}"));

            if let Some(ref pk) = public_key {
                req = req.header("x-ed25519-pubkey", pk);
            }

            let mut params = Vec::new();
            if let Some(ref ch) = channel {
                params.push(("channel", ch.clone()));
            }
            if let Some(a) = after {
                params.push(("after", a.to_string()));
            }
            if let Some(b) = before {
                params.push(("before", b.to_string()));
            }
            if let Some(l) = limit {
                params.push(("limit", l.to_string()));
            }
            if let Some(ref t) = tags {
                params.push(("tags", t.clone()));
            }

            let resp = req
                .query(&params)
                .send()
                .await
                .expect("failed to send request");

            let status = resp.status();
            let body = resp.text().await.expect("failed to read response");

            if !status.is_success() {
                eprintln!("HTTP {status}: {body}");
                std::process::exit(1);
            }

            // Pretty-print the JSON
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            } else {
                println!("{body}");
            }
        }
    }
}
