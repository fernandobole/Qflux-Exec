use anyhow::{bail, Result};
use rust_decimal::Decimal;
use std::collections::BTreeMap;

use crate::instruments::InstrumentRule;

/// Order book en ticks enteros. Un bid/ask se guarda como (ticks: i64, qty: Decimal).
/// Convertimos a precio real multiplicando por tick_size al leer.
#[derive(Debug, Clone)]
pub struct OrderBook {
    symbol: String,
    tick_size: Decimal,
    bids: BTreeMap<i64, Decimal>,
    asks: BTreeMap<i64, Decimal>,
    last_update_ms: Option<i64>,
}

impl OrderBook {
    pub fn new(rule: &InstrumentRule) -> Self {
        Self {
            symbol: rule.symbol.clone(),
            tick_size: rule.tick_size,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_update_ms: None,
        }
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    pub fn tick_size(&self) -> Decimal {
        self.tick_size
    }

    pub fn last_update_ms(&self) -> Option<i64> {
        self.last_update_ms
    }
}

impl OrderBook {
    /// Convierte un precio a ticks enteros. Falla si price no es múltiplo exacto de tick_size
    /// (los libros de Bybit cumplen esto; cualquier violación es basura y debe abortar).
    fn price_to_ticks(&self, price: Decimal) -> Result<i64> {
        if self.tick_size <= Decimal::ZERO {
            bail!("{}: tick_size no positivo ({})", self.symbol, self.tick_size);
        }
        let ratio = price / self.tick_size;
        let rounded = ratio.round();
        if (ratio - rounded).abs() > Decimal::new(1, 9) {
            bail!(
                "{}: precio {} no es múltiplo exacto de tick_size {}",
                self.symbol,
                price,
                self.tick_size
            );
        }
        use rust_decimal::prelude::ToPrimitive;
        rounded
            .to_i64()
            .ok_or_else(|| anyhow::anyhow!("{}: precio {} fuera de rango i64", self.symbol, price))
    }

    fn ticks_to_price(&self, ticks: i64) -> Decimal {
        Decimal::from(ticks) * self.tick_size
    }
}

impl OrderBook {
    pub fn apply_snapshot(
        &mut self,
        bids: &[(Decimal, Decimal)],
        asks: &[(Decimal, Decimal)],
        update_ms: i64,
    ) -> Result<()> {
        self.bids.clear();
        self.asks.clear();
        for (price, qty) in bids {
            if *qty <= Decimal::ZERO {
                continue;
            }
            let ticks = self.price_to_ticks(*price)?;
            self.bids.insert(ticks, *qty);
        }
        for (price, qty) in asks {
            if *qty <= Decimal::ZERO {
                continue;
            }
            let ticks = self.price_to_ticks(*price)?;
            self.asks.insert(ticks, *qty);
        }
        self.last_update_ms = Some(update_ms);
        Ok(())
    }
}

impl OrderBook {
    pub fn apply_delta(
        &mut self,
        bids: &[(Decimal, Decimal)],
        asks: &[(Decimal, Decimal)],
        update_ms: i64,
    ) -> Result<()> {
        for (price, qty) in bids {
            let ticks = self.price_to_ticks(*price)?;
            if *qty <= Decimal::ZERO {
                self.bids.remove(&ticks);
            } else {
                self.bids.insert(ticks, *qty);
            }
        }
        for (price, qty) in asks {
            let ticks = self.price_to_ticks(*price)?;
            if *qty <= Decimal::ZERO {
                self.asks.remove(&ticks);
            } else {
                self.asks.insert(ticks, *qty);
            }
        }
        self.last_update_ms = Some(update_ms);
        Ok(())
    }
}

impl OrderBook {
    pub fn best_bid(&self) -> Option<(Decimal, Decimal)> {
        self.bids
            .iter()
            .next_back()
            .map(|(t, q)| (self.ticks_to_price(*t), *q))
    }

    pub fn best_ask(&self) -> Option<(Decimal, Decimal)> {
        self.asks
            .iter()
            .next()
            .map(|(t, q)| (self.ticks_to_price(*t), *q))
    }

    pub fn mid(&self) -> Option<Decimal> {
        let (b, _) = self.best_bid()?;
        let (a, _) = self.best_ask()?;
        Some((b + a) / Decimal::from(2))
    }

    pub fn spread_bps(&self) -> Option<Decimal> {
        let (b, _) = self.best_bid()?;
        let (a, _) = self.best_ask()?;
        if a <= b {
            return None;
        }
        let mid = (b + a) / Decimal::from(2);
        if mid <= Decimal::ZERO {
            return None;
        }
        Some((a - b) / mid * Decimal::from(10_000))
    }

    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn rule(symbol: &str, tick: Decimal) -> InstrumentRule {
        InstrumentRule {
            symbol: symbol.to_string(),
            tick_size: tick,
            qty_step: dec!(1),
            min_order_qty: dec!(1),
        }
    }

    #[test]
    fn doge_fine_tick_no_colapsa_niveles() {
        let r = rule("DOGEUSDT", dec!(0.00001));
        let mut b = OrderBook::new(&r);
        b.apply_snapshot(
            &[(dec!(0.07914), dec!(100)), (dec!(0.07913), dec!(50))],
            &[(dec!(0.07915), dec!(80)), (dec!(0.07916), dec!(30))],
            1,
        )
        .unwrap();
        assert_eq!(b.bid_levels(), 2);
        assert_eq!(b.ask_levels(), 2);
        assert_eq!(b.best_bid().unwrap().0, dec!(0.07914));
        assert_eq!(b.best_ask().unwrap().0, dec!(0.07915));
    }
}

#[cfg(test)]
mod tests_more {
    use super::*;
    use rust_decimal_macros::dec;

    fn rule(symbol: &str, tick: Decimal) -> InstrumentRule {
        InstrumentRule {
            symbol: symbol.to_string(),
            tick_size: tick,
            qty_step: dec!(1),
            min_order_qty: dec!(1),
        }
    }

    #[test]
    fn delta_qty_cero_elimina_nivel() {
        let r = rule("BTCUSDT", dec!(0.10));
        let mut b = OrderBook::new(&r);
        b.apply_snapshot(
            &[(dec!(60000.10), dec!(1)), (dec!(60000.00), dec!(2))],
            &[(dec!(60000.20), dec!(1))],
            1,
        )
        .unwrap();
        assert_eq!(b.bid_levels(), 2);
        b.apply_delta(&[(dec!(60000.10), dec!(0))], &[], 2).unwrap();
        assert_eq!(b.bid_levels(), 1);
        assert_eq!(b.best_bid().unwrap().0, dec!(60000.00));
    }
}

#[cfg(test)]
mod tests_more2 {
    use super::*;
    use rust_decimal_macros::dec;

    fn rule(symbol: &str, tick: Decimal) -> InstrumentRule {
        InstrumentRule {
            symbol: symbol.to_string(),
            tick_size: tick,
            qty_step: dec!(1),
            min_order_qty: dec!(1),
        }
    }

    #[test]
    fn spread_bps_correcto() {
        let r = rule("ETHUSDT", dec!(0.01));
        let mut b = OrderBook::new(&r);
        b.apply_snapshot(
            &[(dec!(3000.00), dec!(1))],
            &[(dec!(3000.30), dec!(1))],
            1,
        )
        .unwrap();
        let s = b.spread_bps().unwrap();
        assert!(s > dec!(0.99) && s < dec!(1.01), "spread={s}");
    }
}

#[cfg(test)]
mod tests_more3 {
    use super::*;
    use rust_decimal_macros::dec;

    fn rule(symbol: &str, tick: Decimal) -> InstrumentRule {
        InstrumentRule {
            symbol: symbol.to_string(),
            tick_size: tick,
            qty_step: dec!(1),
            min_order_qty: dec!(1),
        }
    }

    #[test]
    fn precio_fuera_de_tick_falla() {
        let r = rule("BTCUSDT", dec!(0.10));
        let mut b = OrderBook::new(&r);
        let err = b
            .apply_snapshot(&[(dec!(60000.05), dec!(1))], &[], 1)
            .unwrap_err();
        assert!(format!("{err}").contains("no es múltiplo exacto"));
    }
}
