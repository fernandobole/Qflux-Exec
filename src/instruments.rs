use anyhow::{anyhow, bail, Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use crate::config::Env;

#[derive(Debug, Clone)]
pub struct InstrumentRule {
    pub symbol: String,
    pub tick_size: Decimal,
    pub qty_step: Decimal,
    pub min_order_qty: Decimal,
}

#[derive(Debug, Clone)]
pub struct Instruments {
    by_symbol: HashMap<String, InstrumentRule>,
}

impl Instruments {
    pub fn get(&self, symbol: &str) -> Option<&InstrumentRule> {
        self.by_symbol.get(symbol)
    }

    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }
}

#[derive(Debug, Deserialize)]
struct BybitEnvelope<T> {
    #[serde(rename = "retCode")]
    ret_code: i64,
    #[serde(rename = "retMsg")]
    ret_msg: String,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct InstrumentsResult {
    list: Vec<RawInstrument>,
    #[serde(rename = "nextPageCursor", default)]
    next_page_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawInstrument {
    symbol: String,
    status: String,
    #[serde(rename = "lotSizeFilter")]
    lot: LotFilter,
    #[serde(rename = "priceFilter")]
    price: PriceFilter,
}

#[derive(Debug, Deserialize)]
struct LotFilter {
    #[serde(rename = "qtyStep")]
    qty_step: String,
    #[serde(rename = "minOrderQty")]
    min_order_qty: String,
}

#[derive(Debug, Deserialize)]
struct PriceFilter {
    #[serde(rename = "tickSize")]
    tick_size: String,
}

pub async fn fetch_instruments(env: Env, required_symbols: &[String]) -> Result<Instruments> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("construir reqwest::Client")?;

    let base = env.rest_base();
    let mut by_symbol: HashMap<String, InstrumentRule> = HashMap::new();
    let mut cursor: Option<String> = None;
    let mut pages: u32 = 0;

    loop {
        let mut url = format!(
            "{}/v5/market/instruments-info?category=linear&limit=1000",
            base
        );
        if let Some(c) = &cursor {
            if !c.is_empty() {
                url.push_str("&cursor=");
                url.push_str(&urlencode(c));
            }
        }

        let resp = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .context("leer cuerpo instruments-info")?;
        if !status.is_success() {
            bail!("instruments-info HTTP {status}: {body}");
        }
        let env_resp: BybitEnvelope<InstrumentsResult> = serde_json::from_str(&body)
            .with_context(|| format!("parsear respuesta instruments-info: {body}"))?;
        if env_resp.ret_code != 0 {
            bail!(
                "instruments-info retCode={} retMsg={}",
                env_resp.ret_code,
                env_resp.ret_msg
            );
        }
        let result = env_resp
            .result
            .ok_or_else(|| anyhow!("instruments-info sin 'result'"))?;

        for raw in result.list {
            if raw.status != "Trading" {
                continue;
            }
            let tick_size = Decimal::from_str(raw.price.tick_size.trim())
                .with_context(|| format!("tickSize inválido para {}: {:?}", raw.symbol, raw.price.tick_size))?;
            let qty_step = Decimal::from_str(raw.lot.qty_step.trim())
                .with_context(|| format!("qtyStep inválido para {}: {:?}", raw.symbol, raw.lot.qty_step))?;
            let min_order_qty = Decimal::from_str(raw.lot.min_order_qty.trim())
                .with_context(|| format!("minOrderQty inválido para {}: {:?}", raw.symbol, raw.lot.min_order_qty))?;
            if tick_size <= Decimal::ZERO || qty_step <= Decimal::ZERO {
                bail!(
                    "filtros no positivos para {}: tick={} qty_step={}",
                    raw.symbol,
                    tick_size,
                    qty_step
                );
            }
            by_symbol.insert(
                raw.symbol.clone(),
                InstrumentRule {
                    symbol: raw.symbol,
                    tick_size,
                    qty_step,
                    min_order_qty,
                },
            );
        }

        pages += 1;
        match result.next_page_cursor {
            Some(c) if !c.is_empty() && Some(&c) != cursor.as_ref() => cursor = Some(c),
            _ => break,
        }
        if pages >= 50 {
            bail!("paginación de instruments-info excedió 50 páginas");
        }
    }

    let mut missing: Vec<String> = required_symbols
        .iter()
        .filter(|s| !by_symbol.contains_key(s.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        missing.sort();
        bail!(
            "símbolos requeridos no encontrados en instruments-info: {:?}",
            missing
        );
    }

    Ok(Instruments { by_symbol })
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}
