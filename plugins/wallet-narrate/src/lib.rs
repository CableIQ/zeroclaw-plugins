//! A ZeroClaw WIT tool plugin: `wallet-narrate`.
//!
//! Fetches recent Solana wallet activity via JSON-RPC and narrates it in
//! natural language: SOL transfers, SPL token transfers, swaps on Jupiter,
//! and program interactions. The RPC endpoint is configurable via the plugin's
//! own jailed config section (`config_read` permission).
//!
//! The pure narration core lives in [`narrate`] with no wasm dependency, so it
//! compiles and tests on the host with a plain `cargo test`; the wasm component
//! reuses the exact same logic through this shim, using `solana-client-wasip2`
//! (which speaks `wasi:http` via `WakiTransport`) for RPC calls.
//!
//! Build:  rustup target add wasm32-wasip2
//!         cargo build --target wasm32-wasip2 --release

pub mod narrate;

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use std::collections::HashMap;

    use crate::narrate::{narrate_wallet, NarrateConfig, DEFAULT_RPC_URL};
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use solana_client_wasip2::RpcClient;
    use zeroclaw::plugin::logging::{
        log_record, LogLevel, PluginAction, PluginEvent, PluginOutcome,
    };

    struct WalletNarrate;

    const PLUGIN_NAME: &str = "wallet-narrate";
    const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
    const TOOL_NAME: &str = "narrate-wallet";

    #[derive(serde::Deserialize)]
    struct ExecuteArgs {
        wallet: String,
        #[serde(default = "default_limit")]
        limit: u32,
        #[serde(rename = "__config", default)]
        config: HashMap<String, String>,
    }

    fn default_limit() -> u32 {
        10
    }

    impl PluginInfo for WalletNarrate {
        fn plugin_name() -> String {
            PLUGIN_NAME.to_string()
        }

        fn plugin_version() -> String {
            PLUGIN_VERSION.to_string()
        }
    }

    impl Tool for WalletNarrate {
        fn name() -> String {
            TOOL_NAME.to_string()
        }

        fn description() -> String {
            "Narrate recent Solana wallet activity in natural language. \
             Fetches the latest transactions for a given wallet address and \
             summarizes them: SOL transfers, SPL token transfers, Jupiter swaps, \
             and program interactions. Configurable RPC endpoint via plugin config."
                .to_string()
        }

        fn parameters_schema() -> String {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "wallet": {
                        "type": "string",
                        "description": "Solana wallet base58 address to narrate."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of recent transactions to analyze (default: 10, max: 50)."
                    }
                },
                "required": ["wallet"]
            })
            .to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let parsed: ExecuteArgs = match serde_json::from_str(&args) {
                Ok(a) => a,
                Err(e) => {
                    emit(
                        PluginAction::Fail,
                        PluginOutcome::Failure,
                        "invalid arguments",
                        None,
                    );
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("invalid arguments: {e}")),
                    });
                }
            };

            let rpc_url = parsed
                .config
                .get("rpc_url")
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_string());

            // RpcClient::new creates a client with the default wasi:http transport
            // (WakiTransport), which works inside the wasm component.
            let client = RpcClient::new(rpc_url);

            match narrate_wallet(&parsed.wallet, parsed.limit, &client) {
                Ok(output) => {
                    emit(
                        PluginAction::Complete,
                        PluginOutcome::Success,
                        "narrated wallet activity",
                        None,
                    );
                    Ok(ToolResult {
                        success: true,
                        output,
                        error: None,
                    })
                }
                Err(e) => {
                    emit(
                        PluginAction::Fail,
                        PluginOutcome::Failure,
                        "narration failed",
                        None,
                    );
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("narration failed: {e}")),
                    })
                }
            }
        }
    }

    fn emit(
        action: PluginAction,
        outcome: PluginOutcome,
        message: &str,
        _txs: Option<usize>,
    ) {
        log_record(
            LogLevel::Info,
            &PluginEvent {
                function_name: "wallet_narrate::tool::execute".to_string(),
                action,
                outcome: Some(outcome),
                duration_ms: None,
                attrs: None,
                message: message.to_string(),
            },
        );
    }

    export!(WalletNarrate);
}
