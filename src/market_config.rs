use polymarket_client_sdk::types::Utc;

// ─── MarketConfig ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MarketConfig {
    pub asset: String,
    pub interval_mins: u32,
}

impl MarketConfig {
    pub fn new(asset: impl Into<String>, interval_mins: u32) -> Self {
        Self {
            asset: asset.into(),
            interval_mins,
        }
    }

    pub fn btc_5m() -> Self {
        Self::new("btc", 5)
    }
    pub fn btc_15m() -> Self {
        Self::new("btc", 15)
    }
    pub fn eth_5m() -> Self {
        Self::new("eth", 5)
    }
    pub fn eth_15m() -> Self {
        Self::new("eth", 15)
    }

    pub fn label(&self) -> String {
        format!("{}-{}", self.asset, self.interval_label())
    }

    pub fn interval_label(&self) -> String {
        match self.interval_mins {
            m if m < 60 || m % 60 != 0 => format!("{m}m"),
            h => format!("{}h", h / 60),
        }
    }

    pub fn interval_secs(&self) -> i64 {
        self.interval_mins as i64 * 60
    }

    pub fn round_down(&self, ts: i64) -> i64 {
        let s = self.interval_secs();
        (ts / s) * s
    }

    /// Slug for the market whose END timestamp is `end_ts`.
    pub fn slug_for_end_ts(&self, end_ts: i64) -> String {
        format!("{}-updown-{}-{}", self.asset, self.interval_label(), end_ts)
    }
}

// ─── Market ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Market {
    pub slug: String,
    pub up_token: String,
    pub down_token: String,
    pub end_time: i64,
}

impl Market {
    pub fn is_expired(&self) -> bool {
        Utc::now().timestamp() >= self.end_time
    }
    pub fn seconds_until_expiry(&self) -> i64 {
        self.end_time - Utc::now().timestamp()
    }
}
