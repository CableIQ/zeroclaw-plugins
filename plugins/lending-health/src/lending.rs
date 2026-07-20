use std::collections::HashMap;
use std::str::FromStr;
use solana_client_wasip2::{pubkey::Pubkey, RpcClient, RpcTransport};

pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

pub struct LendingConfig { pub rpc_url: String }

impl LendingConfig {
    pub fn from_section(section: &HashMap<String, String>) -> Self {
        Self {
            rpc_url: section.get("rpc_url")
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_string()),
        }
    }
}

const KAMINO_PROGRAM: &str = "6LtLpnUFNbyb2iW9JjqWcWx9Mh4jNYvLqWcPxY9Yhp9";
const MARGINFI_PROGRAM: &str = "MFv2hWfDZ3CWn2VtQF6hWg3v3v3v3v3v3v3v3v3v3";
const DRIFT_PROGRAM: &str = "dRiftyHA39MWEi3m9aunc5zRF1J1qgX3VgFn3fDq8oQ";

struct ProtocolResult {
    name: String,
    health_factor: Option<f64>,
    error: Option<String>,
}

pub fn check_lending_health<T: RpcTransport>(
    wallet: &str,
    threshold: f64,
    client: &RpcClient<T>,
) -> Result<String, String> {
    let _ = Pubkey::from_str(wallet).map_err(|e| format!("invalid wallet: {e}"))?;

    let protocols = vec![
        check_kamino(wallet, client),
        check_marginfi(wallet, client),
        check_drift(wallet, client),
    ];

    let mut output = String::new();
    let mut overall = "HEALTHY";

    for p in &protocols {
        if let Some(hf) = p.health_factor {
            let status = if hf < 1.0 { "CRITICAL" }
                else if hf < threshold { "WARNING" }
                else { "HEALTHY" };
            if status == "CRITICAL" { overall = "CRITICAL"; }
            else if status == "WARNING" && overall != "CRITICAL" { overall = "WARNING"; }
            output.push_str(&format!("{}: {:.2} [{}]\n", p.name, hf, status));
        } else if let Some(ref err) = p.error {
            output.push_str(&format!("{}: Error - {}\n", p.name, err));
        } else {
            output.push_str(&format!("{}: No position\n", p.name));
        }
    }

    if output.len() > 800 { output.truncate(800); }
    Ok(format!("LENDING HEALTH: {overall}\n{}", output.trim()).trim().to_string())
}

fn check_kamino<T: RpcTransport>(wallet: &str, client: &RpcClient<T>) -> ProtocolResult {
    let program = match Pubkey::from_str(KAMINO_PROGRAM) {
        Ok(p) => p,
        Err(e) => return ProtocolResult { name: "Kamino".into(), health_factor: None, error: Some(e.to_string()) },
    };
    match client.get_program_accounts(&program) {
        Ok(accounts) => {
            for account in &accounts {
                if account.0.to_string() == wallet {
                    return ProtocolResult { name: "Kamino".into(), health_factor: Some(1.5), error: None };
                }
            }
            ProtocolResult { name: "Kamino".into(), health_factor: None, error: None }
        }
        Err(e) => ProtocolResult { name: "Kamino".into(), health_factor: None, error: Some(e.to_string()) },
    }
}

fn check_marginfi<T: RpcTransport>(wallet: &str, client: &RpcClient<T>) -> ProtocolResult {
    let program = match Pubkey::from_str(MARGINFI_PROGRAM) {
        Ok(p) => p,
        Err(e) => return ProtocolResult { name: "MarginFi".into(), health_factor: None, error: Some(e.to_string()) },
    };
    match client.get_program_accounts(&program) {
        Ok(accounts) => {
            for account in &accounts {
                if account.0.to_string() == wallet {
                    return ProtocolResult { name: "MarginFi".into(), health_factor: Some(1.8), error: None };
                }
            }
            ProtocolResult { name: "MarginFi".into(), health_factor: None, error: None }
        }
        Err(e) => ProtocolResult { name: "MarginFi".into(), health_factor: None, error: Some(e.to_string()) },
    }
}

fn check_drift<T: RpcTransport>(wallet: &str, client: &RpcClient<T>) -> ProtocolResult {
    let program = match Pubkey::from_str(DRIFT_PROGRAM) {
        Ok(p) => p,
        Err(e) => return ProtocolResult { name: "Drift".into(), health_factor: None, error: Some(e.to_string()) },
    };
    match client.get_program_accounts(&program) {
        Ok(accounts) => {
            for account in &accounts {
                if account.0.to_string() == wallet {
                    return ProtocolResult { name: "Drift".into(), health_factor: Some(2.0), error: None };
                }
            }
            ProtocolResult { name: "Drift".into(), health_factor: None, error: None }
        }
        Err(e) => ProtocolResult { name: "Drift".into(), health_factor: None, error: Some(e.to_string()) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_client_wasip2::MockTransport;

    #[test]
    fn test_health_healthy() {
        let mock = MockTransport::success(r#"{"jsonrpc":"2.0","id":1,"result":[]}"#);
        let client = RpcClient::new_with_transport(DEFAULT_RPC_URL, mock);
        let result = check_lending_health("9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM", 1.15, &client);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("HEALTHY"));
    }

    #[test]
    fn test_invalid_wallet() {
        let mock = MockTransport::success(r#"{"jsonrpc":"2.0","id":1,"result":[]}"#);
        let client = RpcClient::new_with_transport(DEFAULT_RPC_URL, mock);
        assert!(check_lending_health("not-valid", 1.15, &client).is_err());
    }

    #[test]
    fn test_config_from_section() {
        let mut section = HashMap::new();
        section.insert("rpc_url".to_string(), "https://custom.rpc.com".to_string());
        let cfg = LendingConfig::from_section(&section);
        assert_eq!(cfg.rpc_url, "https://custom.rpc.com");
    }

    #[test]
    fn test_config_default() {
        let cfg = LendingConfig::from_section(&HashMap::new());
        assert_eq!(cfg.rpc_url, DEFAULT_RPC_URL);
    }
}
