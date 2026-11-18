//! ember-logs: the launcher's log bundle intake. Settings come from the environment (see deploy/ember-logs.env.example).

use std::net::SocketAddr;
use std::sync::Arc;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() {
    let key = var("EMBER_LOGS_KEY").and_then(|k| hex::decode(k.trim()).ok()).filter(|k| k.len() >= 32);
    let Some(key) = key else {
        eprintln!("ember-logs: EMBER_LOGS_KEY must be at least 32 bytes of hex (openssl rand -hex 32)");
        std::process::exit(2);
    };
    let Some(public) = var("EMBER_LOGS_PUBLIC") else {
        eprintln!("ember-logs: EMBER_LOGS_PUBLIC must be the service's https address");
        std::process::exit(2);
    };
    let config = ember_logs::Config {
        dir: var("EMBER_LOGS_DIR").unwrap_or_else(|| "/var/lib/ember-logs".into()).into(),
        budget: var("EMBER_LOGS_BUDGET_GB").and_then(|v| v.parse::<u64>().ok()).unwrap_or(20) << 30,
        webhook: var("EMBER_LOGS_WEBHOOK"),
        key,
        public: public.trim_end_matches('/').to_string(),
        trust_proxy: var("EMBER_LOGS_TRUST_PROXY").is_some_and(|v| v == "1"),
        ..ember_logs::Config::default()
    };
    if let Err(e) = std::fs::create_dir_all(&config.dir) {
        eprintln!("ember-logs: cannot use {}: {e}", config.dir.display());
        std::process::exit(2);
    }
    let listen: SocketAddr =
        var("EMBER_LOGS_LISTEN").unwrap_or_else(|| "127.0.0.1:8790".into()).parse().expect("EMBER_LOGS_LISTEN");
    let state = Arc::new(ember_logs::AppState::new(config));
    let sweeper = state.clone();
    tokio::spawn(async move {
        loop {
            let removed = sweeper.sweep(ember_logs::now());
            if removed > 0 {
                eprintln!("ember-logs: removed {removed} expired bundles");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });
    let app = ember_logs::router(state).into_make_service_with_connect_info::<SocketAddr>();
    let listener = tokio::net::TcpListener::bind(listen).await.expect("bind");
    eprintln!("ember-logs: listening on {listen}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}
