use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use tasklane_orchestrator::host::PluginClient;
use tasklane_orchestrator::{Config, Orchestrator};
use tasklane_queue::Queue;
use tasklane_server::config::Settings;
use tasklane_server::connections::{self, Secrets};
use tasklane_server::{api, console, runtime, AppState};
use tokio::sync::watch;

#[tokio::main]
async fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "serve".into());
    if cmd == "--help" || cmd == "help" {
        println!("tasklane serve     run the daemon (default)\ntasklane migrate   apply database migrations and exit\ntasklane doctor    check the installation (database, plug-ins, git, Docker) and exit\n\nConfiguration is read from environment variables; see docs/operations.md.");
        return;
    }
    if let Err(e) = run(&cmd).await {
        eprintln!("tasklane: {e}");
        std::process::exit(1);
    }
}

async fn run(cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
    let s = Settings::from_env()?;
    if cmd == "doctor" {
        let (pool, err) = match PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(8))
            .connect(&s.database_url)
            .await
        {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let checks = tasklane_server::doctor::run(&s, pool.as_ref(), err).await;
        if !tasklane_server::doctor::print(&checks) {
            std::process::exit(1);
        }
        return Ok(());
    }
    std::fs::create_dir_all(&s.data_dir)?;
    let secrets = Secrets::new(&s.data_dir)?;
    let (admin_token, fresh) = connections::admin_token(s.admin_token.clone(), &secrets)?;
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
    if fresh {
        eprintln!("\n  First start. Sign in to the console with this admin token (shown once; it is kept in {}):\n\n    {admin_token}\n", secrets.path(connections::ADMIN_TOKEN).display());
    }

    let queue = Queue::new(pool.clone(), 3);
    let mut cfg = Config::new(
        format!("tasklane-{}", std::process::id()),
        s.data_dir.clone(),
    );
    cfg.source_name = s.source_name.clone();
    cfg.max_dev_parallel = s.max_dev_parallel;
    cfg.max_tokens_per_ticket = s.max_tokens_per_ticket;
    cfg.max_in_ci = s.max_in_ci;
    cfg.duplicate_model = s.duplicate_model;
    cfg.dev_timeout = std::time::Duration::from_secs(s.dev_timeout_secs);
    cfg.ci_timeout_secs = s.ci_timeout_secs;
    cfg.allow_no_checks = s.allow_no_checks;
    cfg.attribution = s.attribution;
    // what the console connected (an App, the agent's key and isolation) is added to what the environment says
    let gh: Option<connections::GithubConn> = connections::get(&pool, "github").await;
    let agent: connections::AgentSettings =
        connections::get(&pool, "agent").await.unwrap_or_default();
    let source_spec = connections::source_spec(&s.source, gh.as_ref(), &secrets);
    let engine_spec = connections::engine_spec(&s.engine, &agent, &secrets);
    let orch = Arc::new(Orchestrator::new(
        pool.clone(),
        queue,
        PluginClient::spawn(&source_spec)?,
        PluginClient::spawn(&engine_spec)?,
        cfg,
    ));

    let (stop_tx, stop_rx) = watch::channel(false);
    let mut tasks = runtime::spawn_workers(&orch, s.workers, &stop_rx);
    tasks.push(runtime::spawn_reaper(&orch, s.retention_days, &stop_rx));
    tasks.push(runtime::spawn_sync(&orch, s.sync_every, &stop_rx));
    if let Some(spec) = &s.chat {
        let bridge = Arc::new(tasklane_server::chat::ChatBridge {
            orch: orch.clone(),
            chat: PluginClient::spawn(spec)?,
            channel: "telegram".into(),
            user_daily_limit: s.user_daily_limit,
        });
        tasks.push(tokio::spawn(bridge.run(stop_rx.clone())));
    }

    let state = Arc::new(AppState {
        orch: orch.clone(),
        admin_token,
        jwt_secrets: s.jwt_secrets.clone(),
        user_daily_limit: s.user_daily_limit,
        secure_cookies: s.secure_cookies,
        login_throttle: Default::default(),
        secrets,
        base_source: s.source.clone(),
        base_engine: s.engine.clone(),
        public_url: s.public_url.clone(),
        metrics_token: s.metrics_token.clone(),
    });
    let app = api::routes()
        .merge(console::routes(state.clone()))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&s.bind).await?;
    eprintln!(
        "tasklane listening on {} with {} workers",
        s.bind, s.workers
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        eprintln!("shutting down");
        let _ = stop_tx.send(true);
    })
    .await?;
    for t in tasks {
        let _ = t.await;
    }
    Ok(())
}
