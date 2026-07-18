use std::collections::HashMap;

pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

pub trait HttpClient {
    fn post(&self, url: &str, body: &str) -> Result<String, String>;
}

pub struct RiskConfig {
    pub rpc_url: String,
}

impl RiskConfig {
    pub fn from_section(section: &HashMap<String, String>) -> Self {
        Self {
            rpc_url: section
                .get("rpc_url")
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_string()),
        }
    }
}

pub fn assess_token(
    mint: &str,
    cfg: &RiskConfig,
    http: &dyn HttpClient,
) -> Result<String, String> {
    let mint = mint.trim();
    if mint.is_empty() || mint.len() < 32 {
        return Err("invalid mint address".to_string());
    }

    let mint_info = get_account_info(mint, cfg, http)?;
    let supply = get_token_supply(mint, cfg, http)?;
    let holders = get_largest_holders(mint, cfg, http)?;
    let extensions = check_extensions(mint, cfg, http)?;

    let mut reasons: Vec<String> = Vec::new();
    let mut red_flags: Vec<String> = Vec::new();
    let mut amber_flags: Vec<String> = Vec::new();

    let mint_authority = mint_info
        .get("mintAuthority")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if mint_authority.is_empty() || mint_authority == "null" {
        reasons.push("No mint authority (supply is fixed)".to_string());
    } else {
        red_flags.push(format!(
            "Mint authority: {}...",
            &mint_authority[..mint_authority.len().min(8)]
        ));
    }

    let freeze_authority = mint_info
        .get("freezeAuthority")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !freeze_authority.is_empty() && freeze_authority != "null" {
        red_flags.push(format!(
            "Freeze authority: {}...",
            &freeze_authority[..freeze_authority.len().min(8)]
        ));
    }

    let top10_pct = calculate_top10_concentration(&holders, &supply);
    if top10_pct > 90.0 {
        red_flags.push(format!(
            "Top 10 holders: {:.1}% supply (extreme concentration)",
            top10_pct
        ));
    } else if top10_pct > 50.0 {
        amber_flags.push(format!(
            "Top 10 holders: {:.1}% supply (high concentration)",
            top10_pct
        ));
    }

    for ext in &extensions {
        match ext.as_str() {
            "transferHook" => red_flags.push("Transfer hook: arbitrary logic on transfers".into()),
            "transferFee" => red_flags.push("Transfer fee: taxed on each transfer".into()),
            "permanentDelegate" => {
                red_flags.push("Permanent delegate: can drain any holder".into())
            }
            "confidentialTransfers" => amber_flags.push("Confidential transfers enabled".into()),
            "defaultAccountState" => {
                amber_flags.push("Default account state: may start frozen".into())
            }
            "immutableOwner" => reasons.push("Immutable owner (safe)".into()),
            _ => {}
        }
    }

    let rating = if !red_flags.is_empty() {
        "RED"
    } else if !amber_flags.is_empty() {
        "AMBER"
    } else {
        "GREEN"
    };

    let decimals = mint_info
        .get("decimals")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut output = format!(
        "TOKEN RISK: {rating}\nMint: {mint}\nDecimals: {decimals}\nSupply: {}\n",
        format_supply(&supply)
    );

    if !reasons.is_empty() {
        output.push_str(&format!(
            "OK:\n  {}\n",
            reasons.join("\n  ")
        ));
    }
    if !amber_flags.is_empty() {
        output.push_str(&format!(
            "CAUTION:\n  {}\n",
            amber_flags.join("\n  ")
        ));
    }
    if !red_flags.is_empty() {
        output.push_str(&format!(
            "WARNINGS:\n  {}\n",
            red_flags.join("\n  ")
        ));
    }

    Ok(output.trim().to_string())
}

fn get_account_info(
    mint: &str,
    cfg: &RiskConfig,
    http: &dyn HttpClient,
) -> Result<HashMap<String, serde_json::Value>, String> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [mint, { "encoding": "jsonParsed", "commitment": "confirmed" }]
    });
    let text = http.post(&cfg.rpc_url, &req.to_string())?;
    let resp: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    if let Some(err) = resp.get("error") {
        return Err(format!("RPC error: {err}"));
    }
    let obj = resp["result"]["value"]["data"]["parsed"]["info"]
        .as_object()
        .ok_or_else(|| "not a valid token mint".to_string())?;
    Ok(obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

fn get_token_supply(
    mint: &str,
    cfg: &RiskConfig,
    http: &dyn HttpClient,
) -> Result<serde_json::Value, String> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTokenSupply",
        "params": [mint, { "commitment": "confirmed" }]
    });
    let text = http.post(&cfg.rpc_url, &req.to_string())?;
    let resp: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    if let Some(err) = resp.get("error") {
        return Err(format!("RPC error: {err}"));
    }
    Ok(resp["result"]["value"].clone())
}

fn get_largest_holders(
    mint: &str,
    cfg: &RiskConfig,
    http: &dyn HttpClient,
) -> Result<Vec<serde_json::Value>, String> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTokenLargestAccounts",
        "params": [mint, { "commitment": "confirmed" }]
    });
    let text = http.post(&cfg.rpc_url, &req.to_string())?;
    let resp: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    if let Some(err) = resp.get("error") {
        return Err(format!("RPC error: {err}"));
    }
    Ok(resp["result"]["value"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn check_extensions(
    mint: &str,
    cfg: &RiskConfig,
    http: &dyn HttpClient,
) -> Result<Vec<String>, String> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [mint, { "encoding": "jsonParsed", "commitment": "confirmed" }]
    });
    let text = http.post(&cfg.rpc_url, &req.to_string())?;
    let resp: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;

    let mut extensions = Vec::new();
    if let Some(ext_arr) = resp["result"]["value"]["data"]["parsed"]["info"]["extensions"]
        .as_array()
    {
        for ext in ext_arr {
            if let Some(ext_type) = ext["extension"].as_str() {
                extensions.push(ext_type.to_string());
            }
        }
    }
    Ok(extensions)
}

fn calculate_top10_concentration(
    holders: &[serde_json::Value],
    supply: &serde_json::Value,
) -> f64 {
    let total = supply["uiAmount"].as_f64().unwrap_or(0.0);
    if total <= 0.0 {
        return 0.0;
    }
    let top10: f64 = holders
        .iter()
        .take(10)
        .filter_map(|h| h["uiAmount"].as_f64())
        .sum();
    (top10 / total) * 100.0
}

fn format_supply(supply: &serde_json::Value) -> String {
    let amount = supply["uiAmount"].as_f64().unwrap_or(0.0);
    if amount >= 1_000_000.0 {
        format!("{:.2}M", amount / 1_000_000.0)
    } else if amount >= 1_000.0 {
        format!("{:.2}K", amount / 1_000.0)
    } else {
        supply["uiAmountString"]
            .as_str()
            .unwrap_or("0")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct MockHttp {
        responses: HashMap<String, &'static str>,
    }

    impl MockHttp {
        fn token_usdc() -> Self {
            let mut r = HashMap::new();
            r.insert(
                "getAccountInfo".to_string(),
                r#"{"jsonrpc":"2.0","result":{"value":{"data":{"parsed":{"info":{"decimals":6,"freezeAuthority":null,"mintAuthority":null,"supply":"1000000000000000"},"type":"mint"}}}},"id":1}"#,
            );
            r.insert(
                "getTokenSupply".to_string(),
                r#"{"jsonrpc":"2.0","result":{"value":{"amount":"1000000000000000","decimals":6,"uiAmount":1000000000.0,"uiAmountString":"1000000000"}},"id":1}"#,
            );
            r.insert(
                "getTokenLargestAccounts".to_string(),
                r#"{"jsonrpc":"2.0","result":{"value":[{"address":"abc","amount":"900000000000000","decimals":6,"uiAmount":900000000.0,"uiAmountString":"900000000"},{"address":"def","amount":"50000000000000","decimals":6,"uiAmount":50000000.0,"uiAmountString":"50000000"}]},"id":1}"#,
            );
            MockHttp { responses: r }
        }
    }

    impl HttpClient for MockHttp {
        fn post(&self, _url: &str, body: &str) -> Result<String, String> {
            let req: serde_json::Value =
                serde_json::from_str(body).map_err(|e| e.to_string())?;
            let method = req["method"].as_str().unwrap_or("");
            let val = self
                .responses
                .get(method)
                .ok_or_else(|| format!("no mock for {method}"))?;
            Ok(val.to_string())
        }
    }

    #[test]
    fn assesses_green_token() {
        let cfg = RiskConfig::from_section(&HashMap::new());
        let http = MockHttp::token_usdc();
        let result = assess_token(
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            &cfg,
            &http,
        );
        assert!(result.is_ok(), "failed: {:?}", result.err());
        let output = result.unwrap();
        eprintln!("OUTPUT: {output:?}");
        assert!(output.contains("RED"), "should be RED due to concentration 95%");
    }

    #[test]
    fn assess_token_invalid_mint() {
        let cfg = RiskConfig::from_section(&HashMap::new());
        let http = MockHttp::token_usdc();
        let result = assess_token("", &cfg, &http);
        assert!(result.is_err());
    }

    #[test]
    fn config_reads_rpc_url() {
        let mut section = HashMap::new();
        section.insert("rpc_url".to_string(), "https://custom.rpc.com".to_string());
        let cfg = RiskConfig::from_section(&section);
        assert_eq!(cfg.rpc_url, "https://custom.rpc.com");
    }

    #[test]
    fn format_supply_works() {
        let supply = serde_json::json!({
            "amount": "1000000000000",
            "decimals": 6,
            "uiAmount": 1000000.0,
            "uiAmountString": "1000000"
        });
        assert_eq!(format_supply(&supply), "1.00M");
    }

    #[test]
    fn concentration_calculated() {
        let holders = vec![
            serde_json::json!({"address":"a","uiAmount":900000000.0}),
            serde_json::json!({"address":"b","uiAmount":50000000.0}),
        ];
        let supply = serde_json::json!({"uiAmount":1000000000.0});
        let pct = calculate_top10_concentration(&holders, &supply);
        assert!((pct - 95.0).abs() < 0.1);
    }

    #[test]
    fn red_token_with_mint_authority() {
        let mut r = HashMap::new();
        r.insert(
            "getAccountInfo".to_string(),
            r#"{"jsonrpc":"2.0","result":{"value":{"data":{"parsed":{"info":{"decimals":9,"freezeAuthority":"FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp","mintAuthority":"FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp","supply":"999999999999999999"},"type":"mint"}}}},"id":1}"#,
        );
        r.insert(
            "getTokenSupply".to_string(),
            r#"{"jsonrpc":"2.0","result":{"value":{"amount":"999999999999999999","decimals":9,"uiAmount":1000000000.0,"uiAmountString":"1000000000"}},"id":1}"#,
        );
        r.insert(
            "getTokenLargestAccounts".to_string(),
            r#"{"jsonrpc":"2.0","result":{"value":[]},"id":1}"#,
        );
        let http = MockHttp { responses: r };
        let cfg = RiskConfig::from_section(&HashMap::new());
        let result = assess_token(
            "FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp",
            &cfg,
            &http,
        );
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("RED"));
    }
}
