use solana_client_wasip2::{
    pubkey::Pubkey, rpc::config::RpcSignaturesForAddressConfig,
    RpcClient, RpcTransport,
};
use serde_json::Value;
use std::str::FromStr;

pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

pub struct NarrateConfig {
    pub rpc_url: String,
}

impl Default for NarrateConfig {
    fn default() -> Self {
        Self { rpc_url: DEFAULT_RPC_URL.to_string() }
    }
}

pub fn narrate_wallet<T: RpcTransport>(
    wallet: &str,
    limit: u32,
    client: &RpcClient<T>,
) -> Result<String, String> {
    let limit = limit.min(50).max(1);
    let pubkey = Pubkey::from_str(wallet).map_err(|e| format!("invalid wallet address: {e}"))?;

    let config = RpcSignaturesForAddressConfig {
        limit: Some(limit as usize),
        ..Default::default()
    };
    let sig_infos = client
        .get_signatures_for_address_with_config(&pubkey, config)
        .map_err(|e| format!("RPC error fetching signatures: {e}"))?;

    if sig_infos.is_empty() {
        return Ok(format!("Wallet {} has no recent transactions on this RPC endpoint.", wallet));
    }

    let mut sentences: Vec<String> = Vec::new();
    let mut total_chars = 0usize;

    for info in &sig_infos {
        if total_chars >= 1200 { break; }
        let sig = match solana_client_wasip2::signature::Signature::from_str(&info.signature) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let tx = match client.get_transaction(&sig, solana_client_wasip2::rpc::config::UiTransactionEncoding::JsonParsed) {
            Ok(t) => t,
            Err(_) => continue,
        };

        let maybe_sentence = parse_transaction(&tx, wallet);
        if let Some(s) = maybe_sentence {
            let new_chars = total_chars + s.len() + 1;
            if new_chars > 1200 { break; }
            sentences.push(s);
            total_chars = new_chars;
        }
    }

    if sentences.is_empty() {
        return Ok(format!("Wallet {} has recent activity but no parseable transactions were found.", wallet));
    }

    let mut output = format!("Wallet {} recent activity:\n", wallet);
    for s in &sentences { output.push_str(s); output.push('\n'); }
    Ok(output)
}

fn parse_transaction(tx: &solana_client_wasip2::rpc::response::EncodedConfirmedTransactionWithStatusMeta, wallet: &str) -> Option<String> {
    // Convert to Value for the parsing functions
    let value = serde_json::to_value(tx).ok()?;
    try_parse_sol_transfer(&value, wallet)
        .or_else(|| try_parse_spl_transfer(&value, wallet))
        .or_else(|| try_parse_swap(&value))
        .or_else(|| try_parse_program_interaction(&value))
}

/// Extract the meta object from the serialized EncodedConfirmedTransactionWithStatusMeta.
/// With #[serde(flatten)] on transaction field, meta is at top level.
fn get_meta(value: &Value) -> Option<&Value> {
    value.get("meta")
}

/// Extract accountKeys from the serialized form.
/// With #[serde(flatten)], account keys live at transaction.message.accountKeys
fn get_account_keys(value: &Value) -> Option<&Vec<Value>> {
    value.get("transaction")?
        .get("message")?
        .get("accountKeys")?
        .as_array()
}

fn try_parse_sol_transfer(tx: &Value, wallet: &str) -> Option<String> {
    let meta = get_meta(tx)?;
    let pre_balances = meta.get("preBalances")?.as_array()?;
    let post_balances = meta.get("postBalances")?.as_array()?;
    let account_keys = get_account_keys(tx)?;

    // Find our wallet index (JsonParsed: accountKeys are objects with "pubkey")
    let our_idx = account_keys.iter().position(|k| {
        k.get("pubkey").and_then(|v| v.as_str()) == Some(wallet)
    })?;

    if our_idx >= pre_balances.len() || our_idx >= post_balances.len() { return None; }
    let pre = pre_balances[our_idx].as_u64()?;
    let post = post_balances[our_idx].as_u64()?;
    if pre == post { return None; }
    let diff = if post > pre { post - pre } else { pre - post };
    let sol = diff as f64 / 1_000_000_000.0;
    if sol < 0.000_001 { return None; }

    let fee_payer = account_keys.first()?.get("pubkey").and_then(|v| v.as_str())?;
    let is_sender = wallet == fee_payer || pre > post;

    if is_sender {
        for (i, key) in account_keys.iter().enumerate() {
            if i >= post_balances.len() || i >= pre_balances.len() { continue; }
            let addr = key.get("pubkey").and_then(|v| v.as_str())?;
            if addr == wallet || addr == fee_payer { continue; }
            if post_balances[i].as_u64()? > pre_balances[i].as_u64()? {
                return Some(format!("Sent {:.6} SOL to {}", sol, shorten(addr)));
            }
        }
        Some(format!("Sent {:.6} SOL", sol))
    } else {
        let mut sender = fee_payer;
        for (i, key) in account_keys.iter().enumerate() {
            if i >= post_balances.len() || i >= pre_balances.len() { continue; }
            let addr = key.get("pubkey").and_then(|v| v.as_str())?;
            if addr == wallet { continue; }
            if pre_balances[i].as_u64()? > post_balances[i].as_u64()? {
                sender = addr; break;
            }
        }
        Some(format!("Received {:.6} SOL from {}", sol, shorten(sender)))
    }
}

fn try_parse_spl_transfer(tx: &Value, wallet: &str) -> Option<String> {
    let meta = get_meta(tx)?;
    let inner_instructions = meta.get("innerInstructions")?.as_array()?;
    for inner in inner_instructions {
        let instructions = inner.get("instructions")?.as_array()?;
        for instr in instructions {
            let program = instr.get("programId")?.as_str()?;
            if program != "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" { continue; }
            let parsed = instr.get("parsed")?;
            let type_str = parsed.get("type")?.as_str()?;
            if type_str != "transfer" && type_str != "transferChecked" { continue; }
            let info = parsed.get("info")?;
            let source = info.get("source")?.as_str()?;
            let destination = info.get("destination")?.as_str()?;
            let mint = info.get("mint").and_then(|m| m.as_str());
            let token_amount = if type_str == "transferChecked" {
                info.get("tokenAmount")?.get("uiAmount")?.as_f64()?
            } else {
                let raw = info.get("amount")?.as_str()?;
                raw.parse::<f64>().ok()? / 1_000_000.0
            };
            let token_symbol = mint.map(shorten).unwrap_or_else(|| "tokens".to_string());
            if source == wallet {
                return Some(format!("Sent {:.4} {} to {}", token_amount, token_symbol, shorten(destination)));
            } else if destination == wallet {
                return Some(format!("Received {:.4} {} from {}", token_amount, token_symbol, shorten(source)));
            }
        }
    }
    None
}

fn try_parse_swap(tx: &Value) -> Option<String> {
    let meta = get_meta(tx)?;
    let log_messages = meta.get("logMessages")?.as_array()?;
    let combined: String = log_messages.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>().join(" ");
    let is_jupiter = combined.contains("JUP") || combined.contains("jupiter") || combined.contains("Jupiter") || combined.contains("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
    if !is_jupiter { return None; }

    let pre_token_balances = meta.get("preTokenBalances")?.as_array()?;
    let post_token_balances = meta.get("postTokenBalances")?.as_array()?;
    if pre_token_balances.is_empty() || post_token_balances.is_empty() {
        return Some("Performed a swap on Jupiter".to_string());
    }

    let mut in_mint = None; let mut in_amount = 0.0_f64;
    let mut out_mint = None; let mut out_amount = 0.0_f64;
    for pre in pre_token_balances {
        let mint = pre.get("mint")?.as_str()?;
        let ui_amount = pre.get("uiTokenAmount")?.get("uiAmount")?.as_f64()?;
        let post = post_token_balances.iter().find(|p| p.get("mint").and_then(|m| m.as_str()) == Some(mint))?;
        let post_ui = post.get("uiTokenAmount")?.get("uiAmount")?.as_f64()?;
        let diff = ui_amount - post_ui;
        if diff > 0.0001 { in_mint = Some(mint.to_string()); in_amount = diff; }
        else if diff < -0.0001 { out_mint = Some(mint.to_string()); out_amount = -diff; }
    }
    match (in_mint, out_mint) {
        (Some(in_m), Some(out_m)) => Some(format!("Swapped {:.4} {} for {:.4} {} on Jupiter", in_amount, shorten_mint(&in_m), out_amount, shorten_mint(&out_m))),
        _ => Some("Performed a swap on Jupiter".to_string()),
    }
}

fn try_parse_program_interaction(tx: &Value) -> Option<String> {
    let meta = get_meta(tx)?;
    let log_messages = meta.get("logMessages")?.as_array()?;
    for msg in log_messages {
        let text = msg.as_str()?;
        if text.starts_with("Program ") && text.contains(" invoke [1]") {
            let pid = text
                .strip_prefix("Program ")
                .and_then(|s| s.split(' ').next())?;
            if pid.contains("11111111111111111111111111111111")
                || pid.contains("Tokenkeg")
                || pid.contains("JUP")
            {
                return None;
            }
            return Some(format!("Interacted with program {}", shorten(pid)));
        }
    }
    None
}

fn shorten(addr: &str) -> String {
    if addr.len() <= 12 { return addr.to_string(); }
    format!("{}...{}", &addr[..6], &addr[addr.len()-6..])
}

fn shorten_mint(mint: &str) -> String {
    match mint {
        "So11111111111111111111111111111111111111112" => "SOL",
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => "USDC",
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" => "USDT",
        "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263" => "BONK",
        "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So" => "mSOL",
        "J1toso1uCk3QLmjYXTh8iU9j8Y7GPqT3fU2aJqJ4m4N" => "JitoSOL",
        _ => return shorten(mint),
    }.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    struct CyclingTransport { responses: RefCell<Vec<String>> }
    impl CyclingTransport {
        fn new(responses: Vec<impl Into<String>>) -> Self {
            Self { responses: RefCell::new(responses.into_iter().map(|r| r.into()).collect()) }
        }
    }
    impl RpcTransport for CyclingTransport {
        fn post(&self, _url: &str, _body: &str) -> Result<String, solana_client_wasip2::Error> {
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() { panic!("CyclingTransport: no more responses"); }
            Ok(responses.remove(0))
        }
        fn sleep(&self, _duration: std::time::Duration) {}
    }

    fn rpc_response(result: Value) -> String {
        json!({"jsonrpc":"2.0","id":1,"result":result}).to_string()
    }

    fn mock_client(responses: Vec<Value>) -> RpcClient<CyclingTransport> {
        let transport = CyclingTransport::new(responses.into_iter().map(rpc_response).collect::<Vec<_>>());
        RpcClient::new_with_transport(DEFAULT_RPC_URL, transport)
    }

    // Use real valid base58 pubkeys
    const WALLET: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
    const SENDER: &str = "FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp";

    fn mock_sigs(sigs: &[&str]) -> Value {
        let entries: Vec<Value> = sigs.iter().map(|s| {
            let sig = format!("5VERv8NMHbhZRGKMAHV7EonKrJNphszyYBN4K2TjAqik3YphZYoGQSfVC1mTBVmYhaaMMfyDbTPWCa9aNaY{}", s);
            json!({"signature": sig, "slot": 1, "err": null})
        }).collect();
        json!(entries)
    }

    // Mock transaction helpers — return data matching EncodedConfirmedTransactionWithStatusMeta JSON shape
    fn account_key(pk: &str) -> Value {
        json!({"pubkey": pk, "signer": false, "writable": true, "source": "transaction"})
    }

    fn mock_tx(args: serde_json::Value) -> Value {
        // Wrap with the structure getTransaction returns
        args
    }

    #[test]
    fn test_empty_wallet() {
        let client = mock_client(vec![mock_sigs(&[])]);
        let r = narrate_wallet(WALLET, 5, &client);
        assert!(r.is_ok());
        assert!(r.unwrap().contains("no recent transactions"));
    }

    #[test]
    fn test_shorten() {
        assert_eq!(shorten(WALLET).len(), 15);
        assert_eq!(shorten_mint("So11111111111111111111111111111111111111112"), "SOL");
    }

    #[test]
    fn test_parse_sol_transfer_directly() {
        let value = json!({
            "slot": 1,
            "meta": {
                "err": null,
                "fee": 5000,
                "preBalances": [1005000, 0],
                "postBalances": [5000, 1000000],
                "preTokenBalances": [],
                "postTokenBalances": [],
                "innerInstructions": [],
                "logMessages": ["Program 11111111111111111111111111111111 invoke [1]", "Program 11111111111111111111111111111111 success"]
            },
            "transaction": {
                "message": {
                    "accountKeys": [account_key(SENDER), account_key(WALLET)]
                }
            }
        });
        let result = try_parse_sol_transfer(&value, WALLET);
        assert!(result.is_some(), "Expected SOL transfer");
        let text = result.unwrap();
        assert!(text.contains("Received"), "Expected 'Received' in: {text}");
        assert!(text.contains("SOL"));
    }

    #[test]
    fn test_parse_swap_directly() {
        let value = json!({
            "slot": 1,
            "meta": {
                "err": null,
                "fee": 10000,
                "preBalances": [10000000, 5000000, 0],
                "postBalances": [5000000, 5000000, 5000000],
                "preTokenBalances": [{"mint": "So11111111111111111111111111111111111111112", "uiTokenAmount": {"uiAmount": 10.0}}, {"mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "uiTokenAmount": {"uiAmount": 0.0}}],
                "postTokenBalances": [{"mint": "So11111111111111111111111111111111111111112", "uiTokenAmount": {"uiAmount": 9.0}}, {"mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "uiTokenAmount": {"uiAmount": 190.0}}],
                "innerInstructions": [],
                "logMessages": ["Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]", "Program log: Instruction: Swap", "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success"]
            },
            "transaction": {
                "message": {
                    "accountKeys": [account_key("11111111111111111111111111111111"), account_key("JUP6LkbZbjSaBMUDsPPYshUEKqH9HScbCXd5eN8RcKq")]
                }
            }
        });
        let result = try_parse_swap(&value);
        assert!(result.is_some(), "Expected swap");
        let text = result.unwrap();
        assert!(text.contains("Swapped"), "Expected 'Swapped' in: {text}");
    }

    #[test]
    fn test_parse_program_directly() {
        let value = json!({
            "slot": 1,
            "meta": {
                "err": null,
                "fee": 5000,
                "preBalances": [20000000, 10000000],
                "postBalances": [19995000, 10000000],
                "preTokenBalances": [],
                "postTokenBalances": [],
                "innerInstructions": [],
                "logMessages": ["Program CRaT8RmbnLbFgGhtCHQv6oJKNZXKaLRNqnLJmChVwAG invoke [1]", "Program log: Instruction: DepositStake", "Program CRaT8RmbnLbFgGhtCHQv6oJKNZXKaLRNqnLJmChVwAG success"]
            },
            "transaction": {
                "message": {
                    "accountKeys": [account_key("11111111111111111111111111111111"), account_key("CRaT8RmbnLbFgGhtCHQv6oJKNZXKaLRNqnLJmChVwAG")]
                }
            }
        });
        let result = try_parse_program_interaction(&value);
        assert!(result.is_some(), "Expected program interaction");
        let text = result.unwrap();
        assert!(text.contains("Interacted with program"), "Expected 'Interacted' in: {text}");
    }
}
