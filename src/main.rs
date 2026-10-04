mod config;
mod instruments;
mod book;
mod public_ws;

use anyhow::Result;
use config::Config;
use instruments::fetch_instruments;

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

    for sym in &cfg.symbols {
        let rule = instruments
            .get(sym)
            .expect("ya validado por fetch_instruments");
        tracing::info!(
            "{}: tick_size={} qty_step={} min_order_qty={}",
            rule.symbol,
            rule.tick_size,
            rule.qty_step,
            rule.min_order_qty
        );
    }

    Ok(())
}
