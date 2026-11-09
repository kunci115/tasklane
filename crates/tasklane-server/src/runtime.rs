//! Long-running tasks: workers, reaper, sync loop. All stop on the shutdown signal.

use std::sync::Arc;
use std::time::Duration;

use tasklane_orchestrator::{Orchestrator, RunResult};
use tokio::sync::watch;

/// The console's own approver identity (one admin token = one owner).
pub async fn ensure_admin(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO principals (channel, ref, role) VALUES ('console', 'admin', 'owner') ON CONFLICT (channel, ref) DO NOTHING")
        .execute(pool)
        .await?;
    Ok(())
}

async fn sleep_or_stop(d: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => false,
        _ = stop.changed() => true,
    }
}

pub fn spawn_workers(
    orch: &Arc<Orchestrator>,
    n: usize,
    stop: &watch::Receiver<bool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    (0..n)
        .map(|i| {
            let (orch, mut stop) = (orch.clone(), stop.clone());
            tokio::spawn(async move {
                loop {
                    if *stop.borrow() {
                        return;
                    }
                    let wait = match orch.run_one().await {
                        Ok(RunResult::Idle) => Duration::from_secs(2),
                        Ok(RunResult::Waiting) | Ok(RunResult::Retried) => {
                            Duration::from_millis(500)
                        }
                        Ok(_) => Duration::from_millis(50),
                        Err(e) => {
                            eprintln!("worker {i}: {e}");
                            Duration::from_secs(5)
                        }
                    };
                    if sleep_or_stop(wait, &mut stop).await {
                        return;
                    }
                }
            })
        })
        .collect()
}

pub fn spawn_reaper(
    orch: &Arc<Orchestrator>,
    retention_days: i64,
    stop: &watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let (orch, mut stop) = (orch.clone(), stop.clone());
    tokio::spawn(async move {
        let mut tick: u64 = 0;
        loop {
            match orch.queue().reap_detailed().await {
                Ok((r, dead)) => {
                    if r.requeued + r.failed > 0 {
                        eprintln!("reaper: {} requeued, {} failed", r.requeued, r.failed);
                    }
                    for (job, ticket) in dead {
                        if let Err(e) = orch
                            .fail_orphaned(job, ticket, "the worker died or hung on every attempt")
                            .await
                        {
                            eprintln!("reaper: ticket {ticket}: {e}");
                        }
                    }
                }
                Err(e) => eprintln!("reaper: {e}"),
            }
            tick += 1;
            if tick % 120 == 1 {
                // hourly housekeeping: leftover checkouts and old bookkeeping rows
                orch.sweep_workspaces().await;
                if let Err(e) = orch.prune_old_rows(retention_days).await {
                    eprintln!("housekeeping: {e}");
                }
            }
            if sleep_or_stop(Duration::from_secs(30), &mut stop).await {
                return;
            }
        }
    })
}

pub fn spawn_sync(
    orch: &Arc<Orchestrator>,
    every: Duration,
    stop: &watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let (orch, mut stop) = (orch.clone(), stop.clone());
    tokio::spawn(async move {
        loop {
            // nothing to sync until a ticket source is connected; stay quiet instead of logging an error every minute
            if !orch.source_configured().await {
                if sleep_or_stop(every, &mut stop).await {
                    return;
                }
                continue;
            }
            match orch.sync_once().await {
                Ok(r) if r.tickets_created + r.tickets_resumed > 0 => {
                    eprintln!(
                        "sync: {} services, {} new tickets, {} resumed",
                        r.services_seen, r.tickets_created, r.tickets_resumed
                    )
                }
                Ok(_) => {}
                Err(e) => eprintln!("sync: {e}"),
            }
            if sleep_or_stop(every, &mut stop).await {
                return;
            }
        }
    })
}
