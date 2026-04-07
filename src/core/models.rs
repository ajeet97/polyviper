use polymarket_client_sdk::clob::types::Side;

#[derive(Debug, Clone, Copy, Default)]
pub struct BBO {
    pub bid: f64,
    pub ask: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PolymarketBook {
    pub best_bid: f64,
    pub best_bid_size: f64,
    pub best_ask: f64,
    pub best_ask_size: f64,
}

#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub intent_id: String,
    pub market_id: String,
    pub token_id: String,
    pub price: f64,
    pub size: f64,
    pub side: Side,
}

#[derive(Debug, Clone)]
pub enum MarketEvent {
    BookUpdate { market_id: String, token_id: String, book: PolymarketBook },
    CexPriceUpdate { symbol: String, bbo: BBO },
    DvolUpdate { value: f64 },
}
