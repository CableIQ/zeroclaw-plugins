//! Pure wallet-narration core. No wit-bindgen or wasm dependency so it compiles
//! and tests on the host with a plain `cargo test`, while the wasm component
//! reuses the exact same logic through `lib.rs`.
//!
//! Makes JSON-RPC calls to a Solana RPC node to fetch recent transactions for a
//! wallet and summarizes them as natural-language sentences.

use serde_json::Value;

// ---------------------------------------------------------------------------
// HTTP abstraction – swapped for real waki calls in wasm, mocked in tests
// ---------------------------------------------------------------------------

/// Minimal HTTP client trait so the core can be tested without live RPC calls.
pub trait HttpClient {
    /// Perform a POST request against `url` with the given JSON body.
    /// Returns the response body as a `serde_json::Value`.
    fn post_json(&self, url: &str, body: &Value) -> Result<Value, String>;
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

/// Configuration for wallet narration.
pub struct NarrateConfig {
    pub rpc_url: String,
}

impl Default for NarrateConfig {
    fn default() -> Self {
        Self {
            rpc_url: DEFAULT_RPC_URL.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Core narration logic
// ---------------------------------------------------------------------------

/// Maximum number of output characters to aim for (~300 tokens ≈ 1200 chars).
/// We use character count as a rough proxy.
const MAX_OUTPUT_CHARS: usize = 1200;

/// Narrate a Solana wallet's recent activity.
///
/// * `wallet` – base58 address of the wallet to inspect.
/// * `limit`  – max number of recent signatures to fetch (capped at 50).
/// * `cfg`    – narration config (RPC URL).
/// * `http`   – HTTP client abstraction.
///
/// Returns a human-readable summary string.
pub fn narrate_wallet(
    wallet: &str,
    limit: u32,
    cfg: &NarrateConfig,
    http: &dyn HttpClient,
) -> Result<String, String> {
    let limit = limit.min(50).max(1);

    // --- 1. Fetch recent signatures ----------------------------------------
    let sigs = get_signatures(wallet, limit, cfg, http)?;
    if sigs.is_empty() {
        return Ok(format!(
            "Wallet {} has no recent transactions on this RPC endpoint.",
            wallet
        ));
    }

    // --- 2. Fetch full transaction data for each signature ------------------
    let mut sentences: Vec<String> = Vec::new();
    let mut total_chars = 0usize;

    for sig in &sigs {
        if total_chars >= MAX_OUTPUT_CHARS {
            break;
        }

        let tx = match get_transaction(sig, cfg, http) {
            Ok(t) => t,
            Err(_) => continue,
        };

        let maybe_sentence = parse_transaction(&tx, wallet);
        if let Some(s) = maybe_sentence {
            let new_chars = total_chars + s.len() + 1; // +1 for newline
            if new_chars > MAX_OUTPUT_CHARS {
                break;
            }
            sentences.push(s);
            total_chars = new_chars;
        }
    }

    if sentences.is_empty() {
        return Ok(format!(
            "Wallet {} has recent activity but no parseable transactions were found.",
            wallet
        ));
    }

    let mut output = format!("Wallet {} recent activity:\n", wallet);
    for s in &sentences {
        output.push_str(s);
        output.push('\n');
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// RPC helpers
// ---------------------------------------------------------------------------

/// Fetch recent signatures for a wallet address.
fn get_signatures(
    wallet: &str,
    limit: u32,
    cfg: &NarrateConfig,
    http: &dyn HttpClient,
) -> Result<Vec<String>, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSignaturesForAddress",
        "params": [wallet, { "limit": limit }]
    });

    let resp = http.post_json(&cfg.rpc_url, &body)?;

    let result = resp
        .get("result")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "invalid response: missing result array".to_string())?;

    let sigs: Vec<String> = result
        .iter()
        .filter_map(|entry| entry.get("signature").and_then(|s| s.as_str()))
        .map(|s| s.to_string())
        .collect();

    Ok(sigs)
}

/// Fetch a full transaction by signature.
fn get_transaction(
    sig: &str,
    cfg: &NarrateConfig,
    http: &dyn HttpClient,
) -> Result<Value, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [
            sig,
            { "encoding": "jsonParsed", "maxSupportedTransactionVersion": 0 }
        ]
    });

    let resp = http.post_json(&cfg.rpc_url, &body)?;

    let result = resp
        .get("result")
        .ok_or_else(|| "invalid response: missing result".to_string())?;

    if result.is_null() {
        return Err("transaction not found".to_string());
    }

    Ok(result.clone())
}

// ---------------------------------------------------------------------------
// Transaction parser
// ---------------------------------------------------------------------------

/// Try to produce a single natural-language sentence from a parsed transaction.
fn parse_transaction(tx: &Value, wallet: &str) -> Option<String> {
    // Try balance-changing instructions first (pre/post balances or token balances)
    if let Some(s) = parse_sol_transfer(tx, wallet) {
        return Some(s);
    }
    if let Some(s) = parse_spl_transfer(tx, wallet) {
        return Some(s);
    }
    if let Some(s) = parse_swap(tx) {
        return Some(s);
    }
    if let Some(s) = parse_program_interaction(tx) {
        return Some(s);
    }
    None
}

/// Detect a native SOL transfer by comparing pre/post token balances.
fn parse_sol_transfer(tx: &Value, wallet: &str) -> Option<String> {
    let meta = tx.get("meta")?;
    let pre_balances = meta.get("preBalances")?.as_array()?;
    let post_balances = meta.get("postBalances")?.as_array()?;
    let account_keys = tx.get("transaction")?.get("message")?.get("accountKeys")?.as_array()?;

    // Find the index of our wallet
    let our_idx = account_keys.iter().position(|k| {
        k.as_str().map(|s| s == wallet).unwrap_or(false)
    })?;

    if our_idx >= pre_balances.len() || our_idx >= post_balances.len() {
        return None;
    }

    let pre = pre_balances[our_idx].as_u64()?;
    let post = post_balances[our_idx].as_u64()?;

    if pre == post {
        return None;
    }

    let diff = if post > pre { post - pre } else { pre - post };
    let sol = diff as f64 / 1_000_000_000.0;

    if sol < 0.000_001 {
        return None; // negligible
    }

    // Determine recipient: find fee payer & recipient accounts
    let fee_payer = account_keys.first()?.as_str()?;
    let is_sender = wallet == fee_payer || pre > post;

    if is_sender {
        // Find who we sent to: the account that gained SOL
        for (i, key) in account_keys.iter().enumerate() {
            if i >= post_balances.len() || i >= pre_balances.len() {
                continue;
            }
            let addr = key.as_str()?;
            if addr == wallet || addr == fee_payer {
                continue;
            }
            if post_balances[i].as_u64()? > pre_balances[i].as_u64()? {
                let recipient = addr;
                return Some(format!(
                    "Sent {:.6} SOL to {}",
                    sol,
                    shorten(recipient)
                ));
            }
        }
        Some(format!("Sent {:.6} SOL", sol))
    } else {
        // We received SOL
        // Find who sent it: check fee payer or the account that lost SOL
        let mut sender = fee_payer;
        for (i, key) in account_keys.iter().enumerate() {
            if i >= post_balances.len() || i >= pre_balances.len() {
                continue;
            }
            let addr = key.as_str()?;
            if addr == wallet {
                continue;
            }
            if pre_balances[i].as_u64()? > post_balances[i].as_u64()? {
                sender = addr;
                break;
            }
        }
        Some(format!(
            "Received {:.6} SOL from {}",
            sol,
            shorten(sender)
        ))
    }
}

/// Detect an SPL token transfer via the innerInstructions or log messages.
fn parse_spl_transfer(tx: &Value, wallet: &str) -> Option<String> {
    // Look at log messages for SPL Token program transfers
    let meta = tx.get("meta")?;
    let _log_messages = meta.get("logMessages")?.as_array()?;

    // Parse inner instructions for token transfer details
    let inner_instructions = meta.get("innerInstructions")?.as_array()?;

    for inner in inner_instructions {
        let instructions = inner.get("instructions")?.as_array()?;
        for instr in instructions {
            let program = instr.get("programId")?.as_str()?;
            // Token program ID
            if program != "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" {
                continue;
            }

            let parsed = instr.get("parsed")?;
            let type_str = parsed.get("type")?.as_str()?;
            if type_str != "transfer" && type_str != "transferChecked" {
                continue;
            }

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
                return Some(format!(
                    "Sent {:.4} {} to {}",
                    token_amount,
                    token_symbol,
                    shorten(destination)
                ));
            } else if destination == wallet {
                return Some(format!(
                    "Received {:.4} {} from {}",
                    token_amount,
                    token_symbol,
                    shorten(source)
                ));
            }
        }
    }

    None
}

/// Detect a Jupiter swap by examining log messages for swap-related programs.
fn parse_swap(tx: &Value) -> Option<String> {
    let meta = tx.get("meta")?;
    let log_messages = meta.get("logMessages")?.as_array()?;

    let log_text: Vec<&str> = log_messages
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    let combined = log_text.join(" ");

    // Check for Jupiter swap indicators
    let is_jupiter = combined.contains("JUP")
        || combined.contains("jupiter")
        || combined.contains("Jupiter")
        || combined.contains("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");

    if !is_jupiter {
        return None;
    }

    // Try to extract "swapped X for Y" from pre/post token balances
    let pre_token_balances = meta.get("preTokenBalances")?.as_array()?;
    let post_token_balances = meta.get("postTokenBalances")?.as_array()?;

    if pre_token_balances.is_empty() || post_token_balances.is_empty() {
        return Some("Performed a swap on Jupiter".to_string());
    }

    // Find which mint decreased and which increased
    let mut in_mint = None;
    let mut in_amount = 0.0_f64;
    let mut out_mint = None;
    let mut out_amount = 0.0_f64;

    for pre in pre_token_balances {
        let mint = pre.get("mint")?.as_str()?;
        let ui_amount = pre.get("uiTokenAmount")?.get("uiAmount")?.as_f64()?;

        let post = post_token_balances.iter().find(|p| {
            p.get("mint").and_then(|m| m.as_str()) == Some(mint)
        })?;
        let post_ui = post.get("uiTokenAmount")?.get("uiAmount")?.as_f64()?;

        let diff = ui_amount - post_ui;
        if diff > 0.0001 {
            // This mint decreased — input side
            in_mint = Some(mint.to_string());
            in_amount = diff;
        } else if diff < -0.0001 {
            // This mint increased — output side
            out_mint = Some(mint.to_string());
            out_amount = -diff;
        }
    }

    match (in_mint, out_mint) {
        (Some(in_m), Some(out_m)) => {
            let in_sym = shorten_mint(&in_m);
            let out_sym = shorten_mint(&out_m);
            Some(format!(
                "Swapped {:.4} {} for {:.4} {} on Jupiter",
                in_amount, in_sym, out_amount, out_sym
            ))
        }
        _ => Some("Performed a swap on Jupiter".to_string()),
    }
}

/// Generic program interaction fallback.
fn parse_program_interaction(tx: &Value) -> Option<String> {
    let meta = tx.get("meta")?;
    let log_messages = meta.get("logMessages")?.as_array()?;

    // Check the first log message for the program invoke
    for msg in log_messages {
        let text = msg.as_str()?;
        if text.starts_with("Program ") && text.contains(" invoke [1]") {
            let program_id = text
                .strip_prefix("Program ")
                .and_then(|s| s.split(' ').next());
            if let Some(pid) = program_id {
                // Skip well-known programs we already handle
                if pid.contains("11111111111111111111111111111111") // System Program
                    || pid.contains("Tokenkeg") // Token Program
                    || pid.contains("JUP") // Jupiter
                {
                    return None;
                }
                return Some(format!("Interacted with program {}", shorten(pid)));
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Shorten a base58 address to first 6 + last 6 chars.
fn shorten(addr: &str) -> String {
    if addr.len() <= 12 {
        return addr.to_string();
    }
    format!("{}...{}", &addr[..6], &addr[addr.len() - 6..])
}

/// Shorten a mint address to a ticker-like abbreviation.
fn shorten_mint(mint: &str) -> String {
    // Known stable mints
    match mint {
        "So11111111111111111111111111111111111111112" => "SOL".to_string(),
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" => "USDC".to_string(),
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB" => "USDT".to_string(),
        "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263" => "BONK".to_string(),
        "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So" => "mSOL".to_string(),
        "J1toso1uCk3QLmjYXTh8iU9j8Y7GPqT3fU2aJqJ4m4N" => "JitoSOL".to_string(),
        _ => shorten(mint),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Mock HTTP client that returns canned JSON-RPC responses.
    struct MockHttp {
        responses: RefCell<Vec<Value>>,
    }

    impl MockHttp {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                responses: RefCell::new(responses),
            }
        }
    }

    impl HttpClient for MockHttp {
        fn post_json(&self, _url: &str, _body: &Value) -> Result<Value, String> {
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                return Err("no more canned responses".to_string());
            }
            Ok(responses.remove(0))
        }
    }

    // Helper: build a mock getSignaturesForAddress response
    fn mock_sigs_response(sigs: &[&str]) -> Value {
        let entries: Vec<Value> = sigs
            .iter()
            .map(|s| serde_json::json!({"signature": s}))
            .collect();
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": entries
        })
    }

    // Helper: build a mock getTransaction response for a simple SOL transfer
    fn mock_sol_transfer_response(
        sender: &str,
        recipient: &str,
        amount_lamports: u64,
        fee_payer: &str,
    ) -> Value {
        // Simulate pre/post balances
        // Index 0 = fee payer, index 1 = sender, index 2 = recipient
        let keys = vec![fee_payer, sender, recipient];
        let fee: u64 = 5000;

        let mut pre = vec![0u64; keys.len()];
        let mut post = vec![0u64; keys.len()];

        if sender == fee_payer {
            // Sender pays fee
            pre[0] = amount_lamports + fee;
            post[0] = fee;
            pre[2] = 0;
            post[2] = amount_lamports;
        } else {
            pre[0] = fee;
            post[0] = 0;
            let idx = keys.iter().position(|k| *k == sender).unwrap_or(1);
            let ridx = keys.iter().position(|k| *k == recipient).unwrap_or(2);
            pre[idx] = amount_lamports + fee;
            post[idx] = fee;
            pre[ridx] = 0;
            post[ridx] = amount_lamports;
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "slot": 123456,
                "meta": {
                    "err": null,
                    "fee": fee,
                    "preBalances": pre,
                    "postBalances": post,
                    "preTokenBalances": [],
                    "postTokenBalances": [],
                    "innerInstructions": [],
                    "logMessages": [
                        "Program 11111111111111111111111111111111 invoke [1]",
                        "Program 11111111111111111111111111111111 success"
                    ]
                },
                "transaction": {
                    "message": {
                        "accountKeys": keys
                    }
                }
            }
        })
    }

    // Helper: build a mock SOL receive transaction (wallet receives)
    fn mock_sol_receive_response(wallet: &str, sender: &str, amount_lamports: u64) -> Value {
        let mut keys = vec![sender, wallet];
        if sender == wallet {
            keys.push("1111111111111111111111111111111111111112");
        }
        let fee: u64 = 5000;

        let mut pre = vec![0u64; keys.len()];
        let mut post = vec![0u64; keys.len()];

        let s_idx = keys.iter().position(|k| *k == sender).unwrap_or(0);
        let w_idx = keys.iter().position(|k| *k == wallet).unwrap_or(1);

        pre[s_idx] = amount_lamports + fee;
        post[s_idx] = fee; // after fee, lost lamports
        pre[w_idx] = 0;
        post[w_idx] = amount_lamports;

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "slot": 123456,
                "meta": {
                    "err": null,
                    "fee": fee,
                    "preBalances": pre,
                    "postBalances": post,
                    "preTokenBalances": [],
                    "postTokenBalances": [],
                    "innerInstructions": [],
                    "logMessages": [
                        "Program 11111111111111111111111111111111 invoke [1]",
                        "Program 11111111111111111111111111111111 success"
                    ]
                },
                "transaction": {
                    "message": {
                        "accountKeys": keys
                    }
                }
            }
        })
    }

    // Helper: mock a swap transaction on Jupiter
    fn mock_swap_response() -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "slot": 789012,
                "meta": {
                    "err": null,
                    "fee": 10000,
                    "preBalances": [10000000, 5000000, 0],
                    "postBalances": [5000000, 5000000, 5000000],
                    "preTokenBalances": [
                        {
                            "mint": "So11111111111111111111111111111111111111112",
                            "uiTokenAmount": { "uiAmount": 10.0 }
                        },
                        {
                            "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                            "uiTokenAmount": { "uiAmount": 0.0 }
                        }
                    ],
                    "postTokenBalances": [
                        {
                            "mint": "So11111111111111111111111111111111111111112",
                            "uiTokenAmount": { "uiAmount": 9.0 }
                        },
                        {
                            "mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                            "uiTokenAmount": { "uiAmount": 190.0 }
                        }
                    ],
                    "innerInstructions": [],
                    "logMessages": [
                        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P invoke [1]",
                        "Program log: Instruction: Swap",
                        "Program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P success"
                    ]
                },
                "transaction": {
                    "message": {
                        "accountKeys": ["11111111111111111111111111111111", "JUP6LkbZbjSaBMUDsPPYshUEKqH9HScbCXd5eN8RcKq", "So11111111111111111111111111111111111111112"]
                    }
                }
            }
        })
    }

    // Helper: mock a program interaction (not SOL transfer, not token, not swap).
    // The wallet address used (CRaT8...) has zero SOL balance change so
    // parse_sol_transfer will not match, forcing the fallback parser.
    fn mock_program_interaction_response() -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "slot": 456789,
                "meta": {
                    "err": null,
                    "fee": 5000,
                    "preBalances": [20000000, 10000000],
                    "postBalances": [19995000, 10000000],
                    "preTokenBalances": [],
                    "postTokenBalances": [],
                    "innerInstructions": [],
                    "logMessages": [
                        "Program CRaT8RmbnLbFgGhtCHQv6oJKNZXKaLRNqnLJmChVwAG invoke [1]",
                        "Program log: Instruction: DepositStake",
                        "Program CRaT8RmbnLbFgGhtCHQv6oJKNZXKaLRNqnLJmChVwAG success"
                    ]
                },
                "transaction": {
                    "message": {
                        "accountKeys": ["11111111111111111111111111111111", "CRaT8RmbnLbFgGhtCHQv6oJKNZXKaLRNqnLJmChVwAG"]
                    }
                }
            }
        })
    }

    // ---------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------

    #[test]
    fn test_empty_wallet_no_transactions() {
        let cfg = NarrateConfig::default();
        let http = MockHttp::new(vec![serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": []
        })]);

        let result = narrate_wallet("11111111111111111111111111111111", 5, &cfg, &http);
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("no recent transactions"));
    }

    #[test]
    fn test_sol_transfer_sent() {
        let cfg = NarrateConfig::default();
        let wallet = "ABC123XYZ789ABC123XYZ789ABC123XYZ789ABC123";
        let recipient = "REC123XYZ789ABC123XYZ789ABC123XYZ789ABC456";
        let fee_payer = wallet;
        let amount_lamports = 1_000_000_000; // 1 SOL

        let http = MockHttp::new(vec![
            mock_sigs_response(&["sig1"]),
            mock_sol_transfer_response(wallet, recipient, amount_lamports, fee_payer),
        ]);

        let result = narrate_wallet(wallet, 5, &cfg, &http);
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("Sent"), "expected 'Sent' in: {text}");
        assert!(text.contains("SOL"), "expected 'SOL' in: {text}");
    }

    #[test]
    fn test_sol_transfer_received() {
        let cfg = NarrateConfig::default();
        let wallet = "WALLET11111111111111111111111111111111111111";
        let sender = "SENDER22222222222222222222222222222222222222";
        let amount_lamports = 500_000_000; // 0.5 SOL

        let http = MockHttp::new(vec![
            mock_sigs_response(&["sig_recv"]),
            mock_sol_receive_response(wallet, sender, amount_lamports),
        ]);

        let result = narrate_wallet(wallet, 5, &cfg, &http);
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("Received"), "expected 'Received' in: {text}");
        assert!(text.contains("SOL"), "expected 'SOL' in: {text}");
        assert!(
            text.contains(&shorten(sender)),
            "expected sender in: {text}"
        );
    }

    #[test]
    fn test_swap_on_jupiter() {
        let cfg = NarrateConfig::default();
        let wallet = "JUP6LkbZbjSaBMUDsPPYshUEKqH9HScbCXd5eN8RcKq";

        let http = MockHttp::new(vec![
            mock_sigs_response(&["swap_sig"]),
            mock_swap_response(),
        ]);

        let result = narrate_wallet(wallet, 5, &cfg, &http);
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("Swapped"),
            "expected 'Swapped' in: {text}"
        );
        assert!(
            text.contains("Jupiter"),
            "expected 'Jupiter' in: {text}"
        );
    }

    #[test]
    fn test_program_interaction_fallback() {
        let cfg = NarrateConfig::default();
        let wallet = "CRaT8RmbnLbFgGhtCHQv6oJKNZXKaLRNqnLJmChVwAG";

        let http = MockHttp::new(vec![
            mock_sigs_response(&["prog_sig"]),
            mock_program_interaction_response(),
        ]);

        let result = narrate_wallet(wallet, 5, &cfg, &http);
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(
            text.contains("Interacted with program"),
            "expected 'Interacted with program' in: {text}"
        );
    }

    #[test]
    fn test_limit_respected() {
        let cfg = NarrateConfig::default();
        // Return 3 signatures but limit = 1
        let http = MockHttp::new(vec![
            mock_sigs_response(&["sig_a", "sig_b", "sig_c"]),
            // Only first sig should be fetched
            mock_sol_transfer_response(
                "ABC123XYZ789ABC123XYZ789ABC123XYZ789ABC123",
                "REC123XYZ789ABC123XYZ789ABC123XYZ789ABC456",
                500_000_000,
                "ABC123XYZ789ABC123XYZ789ABC123XYZ789ABC123",
            ),
        ]);

        let result = narrate_wallet(
            "ABC123XYZ789ABC123XYZ789ABC123XYZ789ABC123",
            3,
            &cfg,
            &http,
        );
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("SOL"), "expected SOL narration: {text}");
    }

    #[test]
    fn test_shorten_address() {
        let addr = "ABC123XYZ789ABC123XYZ789ABC123XYZ789ABC123";
        let short = shorten(addr);
        assert_eq!(short.len(), 15); // 6 + ... + 6
        assert!(short.starts_with("ABC123"));
        assert!(short.ends_with("C123"));
    }

    #[test]
    fn test_shorten_known_mints() {
        let sol_mint = "So11111111111111111111111111111111111111112";
        let usdc_mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        assert_eq!(shorten_mint(sol_mint), "SOL");
        assert_eq!(shorten_mint(usdc_mint), "USDC");
    }
}
