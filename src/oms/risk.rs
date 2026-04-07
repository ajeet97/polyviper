use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, warn};

pub struct RiskManager {
    pub initial_portfolio_value: f64,
    pub current_portfolio_value: f64,
    pub start_of_day_value: f64,
    pub last_day_reset: u64,

    killswitch_threshold: f64, // For oracle depeg
    consecutive_errors: u32,
    latest_binance: f64,
    latest_coinbase: f64,
}

impl RiskManager {
    pub fn new(initial_portfolio: f64, killswitch_threshold: f64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self {
            initial_portfolio_value: initial_portfolio,
            current_portfolio_value: initial_portfolio,
            start_of_day_value: initial_portfolio,
            last_day_reset: now / 86400,
            killswitch_threshold,
            consecutive_errors: 0,
            latest_binance: 0.0,
            latest_coinbase: 0.0,
        }
    }

    pub fn update_portfolio(&mut self, current_value: f64) {
        self.current_portfolio_value = current_value;
        let today = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 86400;
        if today > self.last_day_reset {
            self.start_of_day_value = current_value;
            self.last_day_reset = today;
        }
    }

    pub fn update_prices(&mut self, binance: f64, coinbase: f64) {
        self.latest_binance = binance;
        self.latest_coinbase = coinbase;
    }

    pub fn is_oracle_depegged(&self) -> bool {
        if self.latest_coinbase == 0.0 || self.latest_binance == 0.0 {
            return true; // Wait for initial prices
        }
        let divergence = (self.latest_binance - self.latest_coinbase).abs();
        divergence > self.killswitch_threshold
    }

    /// Calculates a size based on expected value (edge) but hard caps at 8% of portfolio.
    pub fn calculate_kelly_size(&self, edge: f64) -> f64 {
        // Simplified Kelly: fractional kelly based on pure edge.
        // Real Kelly = W - ((1 - W) / R). For simple binary arb assuming W=edge.
        // Let's use a proxy metric: fraction of portfolio to risk equal to edge itself, up to 8% max.
        let raw_size = self.current_portfolio_value * edge;
        let max_size = self.current_portfolio_value * 0.08; // 8% Max Size Rule

        raw_size.min(max_size).max(1.0) // Minimum $1 order
    }

    pub fn validate_order(&self, size_usd: f64) -> Result<(), &'static str> {
        if self.consecutive_errors > 5 {
            return Err("Killswitch activated: excessive consecutive errors.");
        }

        if self.is_oracle_depegged() {
            return Err("Killswitch activated: Oracle De-Peg detected between Binance/Coinbase.");
        }

        // Daily Drawdown Limit (-20%)
        let daily_pnl =
            (self.current_portfolio_value - self.start_of_day_value) / self.start_of_day_value;
        if daily_pnl <= -0.20 {
            return Err("Daily Stop-Loss Hit: -20% drawdown. Trading halted.");
        }

        // Total Drawdown Kill Switch (-40%)
        let total_pnl = (self.current_portfolio_value - self.initial_portfolio_value)
            / self.initial_portfolio_value;
        if total_pnl <= -0.40 {
            return Err(
                "TOTAL DESTRUCTION KILL SWITCH HIT: -40% from initial investment. Shutting down.",
            );
        }

        let max_allowed_size = self.current_portfolio_value * 0.08;
        if size_usd > max_allowed_size {
            return Err("Order size exceeds the strict 8% MAX_ORDER_SIZE parameter.");
        }

        Ok(())
    }

    pub fn report_error(&mut self) {
        self.consecutive_errors += 1;
    }

    pub fn clear_errors(&mut self) {
        self.consecutive_errors = 0;
    }
}

pub struct VelocityLockout {
    last_prices: std::collections::VecDeque<(Instant, f64)>,
    threshold_percent: f64,
    window: Duration,
}

impl VelocityLockout {
    pub fn new(threshold_percent: f64, window_secs: u64) -> Self {
        Self {
            last_prices: std::collections::VecDeque::new(),
            threshold_percent,
            window: Duration::from_secs(window_secs),
        }
    }

    pub fn update(&mut self, current_price: f64) {
        let now = Instant::now();
        self.last_prices.push_back((now, current_price));
        while let Some(&(t, _)) = self.last_prices.front() {
            if now.duration_since(t) > self.window {
                self.last_prices.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn is_locked(&self) -> bool {
        if let (Some(&(_, first_p)), Some(&(_, last_p))) =
            (self.last_prices.front(), self.last_prices.back())
        {
            if first_p == 0.0 {
                return false;
            }
            let velocity = ((last_p - first_p) / first_p).abs();
            if velocity > self.threshold_percent {
                warn!(
                    "Velocity Lockout active: price moved {:.2}%",
                    velocity * 100.0
                );
                return true;
            }
        }
        false
    }
}
