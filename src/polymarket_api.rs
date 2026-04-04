use anyhow::Context;
use polymarket_client_sdk::types::DateTime;

use crate::market_finder::Market;

const GAMMA_API: &str = "https://gamma-api.polymarket.com";
const CLOB_API: &str = "https://clob.polymarket.com";

#[derive(Debug, Clone, serde::Deserialize)]
struct GammaMarket {
    #[serde(rename = "endDate")]
    end_date: Option<String>,
    outcomes: Option<String>,
    #[serde(rename = "clobTokenIds", default)]
    clob_token_ids: Option<String>,
}

pub async fn fetch_market_by_slug(
    client: &reqwest::Client,
    slug: &str,
) -> Result<Option<Market>, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/markets?slug={}", GAMMA_API, slug);

    let markets: Vec<GammaMarket> = client
        .get(&url)
        .send()
        .await
        .context("Gamma API request failed")?
        .json()
        .await
        .context("GammaMarket json parse failed")?;

    if let Some(market) = markets.first() {
        let end_time = market
            .end_date
            .as_deref()
            .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
            .map(|dt| dt.timestamp())
            .unwrap_or(0);

        let outcomes: Vec<String> = match &market.outcomes {
            Some(v) => serde_json::from_str(v).context("failed to parse outcomes")?,
            _ => return Ok(None),
        };

        let token_ids: Vec<String> = match &market.clob_token_ids {
            Some(v) => serde_json::from_str(v).context("failed to parse clob_token_ids")?,
            _ => return Ok(None),
        };

        let mut up_token = None;
        let mut down_token = None;

        for (idx, outcome) in outcomes.iter().enumerate().take(2) {
            let normalized = outcome.to_lowercase();
            match normalized.as_str() {
                "up" | "yes" => up_token = Some(token_ids[idx].clone()),
                "down" | "no" => down_token = Some(token_ids[idx].clone()),
                _ => (),
            }
        }

        // fallback when order in `outcomes` is not explicit
        if up_token.is_none() && down_token.is_none() {
            up_token = Some(token_ids[0].clone());
            down_token = Some(token_ids[1].clone());
        }

        let (up_token, down_token) = match (up_token, down_token) {
            (Some(u), Some(d)) => (u, d),
            _ => return Ok(None),
        };

        return Ok(Some(Market {
            slug: slug.to_owned(),
            up_token,
            down_token,
            end_time,
        }));
    }

    Ok(None)
}
