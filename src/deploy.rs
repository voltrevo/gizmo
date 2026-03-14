//! Standalone deploy server for gizmo.
//!
//! Runs as a separate process/service so that restarting gizmo doesn't kill
//! the deploy stream. Accepts POST /deploy with a bearer token, runs
//! git pull + cargo build + systemctl restart, and streams output back.
//!
//! Usage: gizmo-deploy --port 10422 --token <TOKEN> --project-dir /path/to/gizmo

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use clap::Parser;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "gizmo-deploy", about = "Standalone deploy server for gizmo")]
struct Cli {
    /// Port to listen on
    #[arg(long, default_value_t = 10_422)]
    port: u16,

    /// Bearer token for authentication
    #[arg(long, env = "GIZMO_DEPLOY_TOKEN")]
    token: String,

    /// Path to the gizmo project directory (containing Cargo.toml)
    #[arg(long, env = "GIZMO_PROJECT_DIR")]
    project_dir: String,
}

struct AppState {
    token: String,
    project_dir: String,
}

fn extract_bearer(headers: &HeaderMap, expected: &str) -> Result<(), StatusCode> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let token = auth
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if token != expected {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

async fn health_handler() -> &'static str {
    "ok"
}

async fn deploy_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(status) = extract_bearer(&headers, &state.token) {
        return (status, "unauthorized").into_response();
    }

    let project_dir = state.project_dir.clone();

    let stream = async_stream::stream! {
        use tokio::io::AsyncBufReadExt;

        eprintln!("deploy: starting in {project_dir}");
        yield Ok::<_, std::io::Error>(format!("deploy: starting in {project_dir}\n"));

        let mut child = match tokio::process::Command::new("bash")
            .arg("-c")
            .arg("source $HOME/.cargo/env 2>/dev/null; set -ex; git pull && cargo build --release 2>&1 && systemctl --user restart gizmo")
            .current_dir(&project_dir)
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                let msg = format!("deploy: failed to spawn: {e}\n");
                eprintln!("{msg}");
                yield Ok(msg);
                return;
            }
        };

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let stdout_lines = tokio::io::BufReader::new(stdout).lines();
        let stderr_lines = tokio::io::BufReader::new(stderr).lines();

        let stdout_stream = tokio_stream::wrappers::LinesStream::new(stdout_lines);
        let stderr_stream = tokio_stream::wrappers::LinesStream::new(stderr_lines);

        use tokio_stream::StreamExt;
        let mut merged = stdout_stream.merge(stderr_stream);

        while let Some(line) = merged.next().await {
            match line {
                Ok(l) => {
                    eprintln!("{l}");
                    yield Ok(format!("{l}\n"));
                }
                Err(e) => yield Ok(format!("stream error: {e}\n")),
            }
        }

        match child.wait().await {
            Ok(status) if status.success() => {
                eprintln!("deploy: success");
                yield Ok("deploy: success\n".to_string());
            }
            Ok(status) => {
                let msg = format!("deploy: failed (exit {status})\n");
                eprintln!("{msg}");
                yield Ok(msg);
            }
            Err(e) => {
                let msg = format!("deploy: wait error: {e}\n");
                eprintln!("{msg}");
                yield Ok(msg);
            }
        }
    };

    Body::from_stream(stream).into_response()
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Verify project dir exists.
    if !std::path::Path::new(&cli.project_dir)
        .join("Cargo.toml")
        .exists()
    {
        eprintln!(
            "error: {} does not contain Cargo.toml",
            cli.project_dir
        );
        std::process::exit(1);
    }

    let state = Arc::new(AppState {
        token: cli.token,
        project_dir: cli.project_dir,
    });

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/deploy", post(deploy_handler))
        .with_state(state);

    let addr = format!("127.0.0.1:{}", cli.port);
    eprintln!("gizmo-deploy listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
