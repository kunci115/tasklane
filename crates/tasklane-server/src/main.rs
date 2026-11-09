use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use tasklane_orchestrator::host::PluginClient;
use tasklane_orchestrator::{Config, Orchestrator};
use tasklane_queue::Queue;
use tasklane_server::config::Settings;
use tasklane_server::{api, console, runtime, AppState};
use tokio::sync::watch;

#[tokio::main]
async fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "serve".into());
    if cmd == "--help" || cmd == "help" {
        println!("tasklane serve     run the daemon (default)\ntasklane migrate   apply database migrations and exit\n\nConfiguration is read from environment variables; see docs/operations.md.");
        return;
    }
    if let Err(e) = run(&cmd).await {
        eprintln!("tasklane: {e}");
        std::process::exit(1);
    }
}

async fn run(cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
    let s = Settings::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(&s.database_url)
        .await?;
    tasklane_orchestrator::migrate(&pool).await?;
    if cmd == "migrate" {
        eprintln!("migrations applied");
        return Ok(());
    }
    runtime::ensure_admin(&pool).await?;

    let queue = Queue::new(pool.clone(), 3);
    let mut cfg = Config::new(
        format!("tasklane-{}", std::process::id()),
        s.data_dir.clone(),
    );
    cfg.source_name = s.source_name.clone();
    cfg.max_dev_parallel = s.max_dev_parallel;
    cfg.allow_no_checks = s.allow_no_checks;
    let orch = Arc::new(Orchestrator::new(
        pool.clone(),
        queue,
        PluginClient::spawn(&s.source)?,
        PluginClient::spawn(&s.engine)?,
        cfg,
    ));

    let (stop_tx, stop_rx) = watch::channel(false);
    let mut tasks = runtime::spawn_workers(&orch, s.workers, &stop_rx);
    tasks.push(runtime::spawn_reaper(&orch, &stop_rx));
    tasks.push(runtime::spawn_sync(&orch, s.sync_every, &stop_rx));

    let state = Arc::new(AppState {
        orch: orch.clone(),
        admin_token: s.admin_token.clone(),
        jwt_secrets: s.jwt_secrets.clone(),
        user_daily_limit: s.user_daily_limit,
        secure_cookies: s.secure_cookies,
    });
    let app = api::routes().merge(console::routes()).with_state(state);
    let listener = tokio::net::TcpListener::bind(&s.bind).await?;
    eprintln!(
        "tasklane listening on {} with {} workers",
        s.bind, s.workers
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("shutting down");
            let _ = stop_tx.send(true);
        })
        .await?;
    for t in tasks {
        let _ = t.await;
    }
    Ok(())
}
