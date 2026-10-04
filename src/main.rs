mod config;
mod instruments;
mod book;
mod public_ws;

use anyhow::Result;
use std::time::Duration;
use tokio::sync::mpsc;

use config::Config;
use instruments::fetch_instruments;
use public_ws::{new_books, run_public_ws, BookEvent};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::from_env()?;
    tracing::info!("{}", cfg.redacted());

    tracing::info!("descargando reglas de instrumentos desde Bybit…");
    let instruments = fetch_instruments(cfg.env, &cfg.symbols).await?;
    tracing::info!("instrumentos totales: {}", instruments.len());

    let books = new_books(&instruments, &cfg.symbols)?;
    let (tx, mut rx) = mpsc::channel::<BookEvent>(1024);

    let ws_books = books.clone();
    let ws_symbols = cfg.symbols.clone();
    let ws_env = cfg.env;
    let ws_task = tokio::spawn(async move {
        if let Err(e) = run_public_ws(ws_env, ws_symbols, ws_books, tx).await {
            tracing::error!("ws terminó con error: {e}");
        }
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;

    let mut events_total: u64 = 0;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            maybe = rx.recv() => {
                if maybe.is_some() { events_total += 1; }
            }
            _ = tick.tick() => {
                let snap = books.lock().await;
                for sym in &cfg.symbols {
                    if let Some(book) = snap.get(sym) {
                        match (book.best_bid(), book.best_ask(), book.spread_bps()) {
                            (Some((bb, _)), Some((ba, _)), Some(sp)) => {
                                tracing::info!(
                                    "{sym}: bid={bb} ask={ba} spread_bps={:.4} bids={} asks={}",
                                    sp,
                                    book.bid_levels(),
                                    book.ask_levels()
                                );
                            }
                            _ => tracing::info!("{sym}: libro vacío"),
                        }
                    }
                }
            }
        }
    }

    tracing::info!("eventos consumidos del canal: {events_total}");
    ws_task.abort();
    Ok(())
}
