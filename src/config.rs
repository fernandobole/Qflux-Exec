use anyhow::{anyhow, bail, Context, Result};
use rust_decimal::Decimal;
use std::env;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    Testnet,
    Mainnet,
}

impl Env {
    pub fn rest_base(self) -> &'static str {
        match self {
            Env::Testnet => "https://api-testnet.bybit.com",
            Env::Mainnet => "https://api.bybit.com",
        }
    }
    pub fn public_ws(self) -> &'static str {
        match self {
            Env::Testnet => "wss://stream-testnet.bybit.com/v5/public/linear",
            Env::Mainnet => "wss://stream.bybit.com/v5/public/linear",
        }
    }
    pub fn private_ws(self) -> &'static str {
        match self {
            Env::Testnet => "wss://stream-testnet.bybit.com/v5/private",
            Env::Mainnet => "wss://stream.bybit.com/v5/private",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub env: Env,
    pub api_key: String,
    pub api_secret: String,
    pub symbols: Vec<String>,
    pub notional_usd: Decimal,
    pub recv_window_ms: u64,
    pub csv_path: String,
}

fn require(name: &str) -> Result<String> {
    env::var(name)
        .map_err(|_| anyhow!("variable de entorno {name} ausente"))
        .and_then(|v| {
            if v.trim().is_empty() {
                Err(anyhow!("variable de entorno {name} vacía"))
            } else {
                Ok(v)
            }
        })
}

fn parse_env(raw: &str) -> Result<Env> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "testnet" => Ok(Env::Testnet),
        "mainnet" => Ok(Env::Mainnet),
        other => bail!("BYBIT_ENV inválido: {other:?} (esperado 'testnet' o 'mainnet')"),
    }
}

fn parse_symbols(raw: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    if out.is_empty() {
        bail!("SYMBOLS vacío: declará al menos un símbolo");
    }
    out.sort();
    out.dedup();
    for s in &out {
        if !s.chars().all(|c| c.is_ascii_alphanumeric()) {
            bail!("símbolo inválido en SYMBOLS: {s:?} (solo letras y dígitos ASCII)");
        }
    }
    Ok(out)
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let env = parse_env(&require("BYBIT_ENV")?)?;
        let api_key = require("BYBIT_API_KEY").context("se requiere BYBIT_API_KEY")?;
        let api_secret = require("BYBIT_API_SECRET").context("se requiere BYBIT_API_SECRET")?;
        let symbols = parse_symbols(&require("SYMBOLS")?)?;

        let notional_raw = require("NOTIONAL_USD")?;
        let notional_usd = Decimal::from_str(notional_raw.trim())
            .with_context(|| format!("NOTIONAL_USD no es un decimal válido: {notional_raw:?}"))?;
        if notional_usd <= Decimal::ZERO {
            bail!("NOTIONAL_USD debe ser > 0 (recibido {notional_usd})");
        }

        let recv_window_ms: u64 = env::var("RECV_WINDOW_MS")
            .ok()
            .map(|v| v.parse::<u64>())
            .transpose()
            .context("RECV_WINDOW_MS debe ser un entero sin signo")?
            .unwrap_or(5000);
        if !(1000..=20000).contains(&recv_window_ms) {
            bail!("RECV_WINDOW_MS fuera de rango [1000,20000]: {recv_window_ms}");
        }

        let csv_path = env::var("CSV_PATH").unwrap_or_else(|_| "./fills.csv".to_string());

        Ok(Self {
            env,
            api_key,
            api_secret,
            symbols,
            notional_usd,
            recv_window_ms,
            csv_path,
        })
    }

    pub fn redacted(&self) -> String {
        format!(
            "Config {{ env: {:?}, api_key: {}***, symbols: {:?}, notional_usd: {}, recv_window_ms: {}, csv_path: {:?} }}",
            self.env,
            &self.api_key.chars().take(4).collect::<String>(),
            self.symbols,
            self.notional_usd,
            self.recv_window_ms,
            self.csv_path,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_ok() {
        assert_eq!(parse_env("testnet").unwrap(), Env::Testnet);
        assert_eq!(parse_env("MAINNET").unwrap(), Env::Mainnet);
    }

    #[test]
    fn parse_env_rejects_default() {
        assert!(parse_env("").is_err());
        assert!(parse_env("prod").is_err());
    }

    #[test]
    fn parse_symbols_dedups_and_uppercases() {
        let s = parse_symbols(" btcusdt , ethusdt, BTCUSDT ").unwrap();
        assert_eq!(s, vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
    }

    #[test]
    fn parse_symbols_rejects_empty() {
        assert!(parse_symbols("  ,  ").is_err());
    }

    #[test]
    fn parse_symbols_rejects_invalid_chars() {
        assert!(parse_symbols("btc-usdt").is_err());
    }
}
