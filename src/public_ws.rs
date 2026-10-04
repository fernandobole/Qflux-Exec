use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use crate::book::OrderBook;
use crate::config::Env;
use crate::instruments::Instruments;

/// Evento publicado al canal cuando se aplica un snapshot o delta.
#[derive(Debug, Clone)]
pub enum BookEvent {
    Updated { symbol: String, update_ms: i64 },
}

fn parse_levels(v: &Value) -> Result<Vec<(Decimal, Decimal)>> {
    let arr = v.as_array().ok_or_else(|| anyhow!("niveles no son array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for lvl in arr {
        let a = lvl
            .as_array()
            .ok_or_else(|| anyhow!("nivel individual no es array"))?;
        if a.len() < 2 {
            bail!("nivel con menos de 2 elementos");
        }
        let p = a[0].as_str().ok_or_else(|| anyhow!("price no string"))?;
        let q = a[1].as_str().ok_or_else(|| anyhow!("qty no string"))?;
        let price = Decimal::from_str(p).with_context(|| format!("price inválido: {p}"))?;
        let qty = Decimal::from_str(q).with_context(|| format!("qty inválido: {q}"))?;
        out.push((price, qty));
    }
    Ok(out)
}

/// Mapa compartido de order books por símbolo.
/// Un `Mutex` por todo el mapa alcanza para esta escala (10 símbolos, un solo lector por ahora).
/// Cuando el executor se enganche, podemos pasar a `DashMap` o un book por tarea si hay contención.
pub type Books = Arc<Mutex<HashMap<String, OrderBook>>>;

pub fn new_books(instruments: &Instruments, symbols: &[String]) -> Result<Books> {
    let mut map = HashMap::with_capacity(symbols.len());
    for s in symbols {
        let rule = instruments
            .get(s)
            .ok_or_else(|| anyhow!("instrumento ausente para {s} (ya validado antes, bug)"))?;
        map.insert(s.clone(), OrderBook::new(rule));
    }
    Ok(Arc::new(Mutex::new(map)))
}

/// Conecta al WS público, se suscribe a orderbook.50.<sym> para cada símbolo,
/// aplica snapshots/deltas al libro y publica eventos BookUpdated al canal.
/// Devuelve cuando la conexión se cierra o hay un error irrecuperable; el caller decide
/// si reintentar (típicamente con backoff).
pub async fn run_public_ws(
    env: Env,
    symbols: Vec<String>,
    books: Books,
    tx: mpsc::Sender<BookEvent>,
) -> Result<()> {
    let url = env.public_ws();
    tracing::info!("conectando WS público: {url}");
    let (ws, _) = connect_async(url)
        .await
        .with_context(|| format!("connect_async {url}"))?;
    let (mut write, mut read) = ws.split();

    // Suscripción: Bybit v5 acepta hasta 10 args por mensaje; mandamos uno por símbolo
    // (margen de sobra) para que el error de uno no tire la suscripción del resto.
    for sym in &symbols {
        let msg = serde_json::json!({
            "op": "subscribe",
            "args": [format!("orderbook.50.{}", sym)],
        });
        write
            .send(Message::Text(msg.to_string().into()))
            .await
            .with_context(|| format!("subscribe {sym}"))?;
    }
    tracing::info!("suscripto a orderbook.50 para {} símbolos", symbols.len());

    let mut ping_timer = tokio::time::interval(Duration::from_secs(20));
    ping_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_timer.tick().await; // consumir el primer tick inmediato

    loop {
        tokio::select! {
            _ = ping_timer.tick() => {
                let ping = serde_json::json!({"op": "ping"}).to_string();
                if let Err(e) = write.send(Message::Text(ping.into())).await {
                    bail!("ping ws falló: {e}");
                }
            }

            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => bail!("read ws error: {e}"),
                    None => bail!("read ws cerrado"),
                };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Ping(p) => {
                        let _ = write.send(Message::Pong(p)).await;
                        continue;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(_) => bail!("ws cerrado por el servidor"),
                    _ => continue,
                };
                let v: Value = match serde_json::from_str(text.as_str()) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("mensaje no-JSON ignorado: {e}");
                        continue;
                    }
                };

                let topic = match v.get("topic").and_then(|t| t.as_str()) {
                    Some(t) => t,
                    None => {
                        if v.get("success").and_then(|s| s.as_bool()) == Some(false) {
                            tracing::warn!("op falló: {v}");
                        }
                        continue;
                    }
                };
                if !topic.starts_with("orderbook.") {
                    continue;
                }
                let sym = match topic.rsplit('.').next() {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => continue,
                };
                let kind = v.get("type").and_then(|k| k.as_str()).unwrap_or("");
                let data = match v.get("data") {
                    Some(d) => d,
                    None => continue,
                };
                let update_ms = v.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
                let bids = parse_levels(&data["b"]).unwrap_or_default();
                let asks = parse_levels(&data["a"]).unwrap_or_default();

                {
                    let mut books_lock = books.lock().await;
                    let book = match books_lock.get_mut(&sym) {
                        Some(b) => b,
                        None => continue,
                    };
                    let res = match kind {
                        "snapshot" => book.apply_snapshot(&bids, &asks, update_ms),
                        "delta" => book.apply_delta(&bids, &asks, update_ms),
                        other => {
                            tracing::warn!("tipo desconocido {other} en {sym}, ignorado");
                            continue;
                        }
                    };
                    if let Err(e) = res {
                        tracing::error!("aplicando {kind} a {sym}: {e}");
                        continue;
                    }
                }

                let _ = tx.try_send(BookEvent::Updated { symbol: sym, update_ms });
            }
        }
    }
}
