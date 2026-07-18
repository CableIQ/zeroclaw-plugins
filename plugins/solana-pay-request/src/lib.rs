//! A ZeroClaw WIT tool plugin: `solana-pay-request`.
//!
//! Generates Solana Pay transfer request URLs (`solana:...`) for on-chain
//! transfer transactions. The URL can be rendered as a QR code to let any
//! Solana wallet initiate the payment. No RPC calls are made — this is pure
//! URL construction.
//!
//! The default SPL token mint is USDC on mainnet
//! (`EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`); the recipient address
//! and amount are required. Optional memo and reference tags can be appended.
//!
//! The pure URL construction core lives in [`pay`] with no wasm dependency,
//! so it compiles and tests on the host with a plain `cargo test`; the wasm
//! component reuses the exact same logic through this shim.
//!
//! Build:  rustup target add wasm32-wasip2
//!         cargo build --target wasm32-wasip2 --release

pub mod pay;

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use crate::pay::generate_pay_url;
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::logging::{
        log_record, LogLevel, PluginAction, PluginEvent, PluginOutcome,
    };

    struct SolanaPayRequest;

    const PLUGIN_NAME: &str = "solana-pay-request";
    const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
    const TOOL_NAME: &str = "solana-pay-request";

    /// Default USDC mint on Solana mainnet.
    const DEFAULT_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    #[derive(serde::Deserialize)]
    struct ExecuteArgs {
        /// Recipient Solana address (base58).
        recipient: String,
        /// Amount in token decimal units (e.g. 1.50 for 1.50 USDC).
        amount: f64,
        /// Optional SPL token mint. Defaults to USDC mainnet if absent.
        #[serde(default = "default_mint")]
        mint: String,
        /// Optional memo text.
        #[serde(default)]
        memo: Option<String>,
        /// Optional reference tag (a base58-encoded 32-byte pubkey).
        #[serde(default)]
        reference: Option<String>,
        /// Injected by the host from this plugin's `[solana-pay-request]` config
        /// section (if the `config_read` permission is granted). Keys match the
        /// field names above; the user-supplied `execute` arguments take
        /// precedence.
        #[serde(rename = "__config", default)]
        config: std::collections::HashMap<String, String>,
    }

    fn default_mint() -> String {
        DEFAULT_MINT.to_string()
    }

    impl PluginInfo for SolanaPayRequest {
        fn plugin_name() -> String {
            PLUGIN_NAME.to_string()
        }

        fn plugin_version() -> String {
            PLUGIN_VERSION.to_string()
        }
    }

    impl Tool for SolanaPayRequest {
        fn name() -> String {
            TOOL_NAME.to_string()
        }

        fn description() -> String {
            "Generate a Solana Pay transfer request URL that any Solana wallet \
             can scan via QR code to initiate a payment. Requires a recipient \
             address and amount. Optionally override the SPL token mint (defaults \
             to USDC on mainnet), add a memo, or attach a reference tag for \
             payment tracking."
                .to_string()
        }

        fn parameters_schema() -> String {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "recipient": {
                        "type": "string",
                        "description": "The recipient's Solana address (base58)."
                    },
                    "amount": {
                        "type": "number",
                        "description": "Transfer amount in token decimal units (e.g. 1.50 for 1.50 USDC)."
                    },
                    "mint": {
                        "type": "string",
                        "description": "SPL token mint address (defaults to USDC on mainnet)."
                    },
                    "memo": {
                        "type": "string",
                        "description": "Optional memo text to include in the transaction."
                    },
                    "reference": {
                        "type": "string",
                        "description": "Optional reference tag (base58-encoded 32-byte pubkey) for payment tracking."
                    }
                },
                "required": ["recipient", "amount"]
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

            let mint = if !parsed.mint.is_empty() && parsed.mint != DEFAULT_MINT {
                // User-supplied value takes precedence
                parsed.mint
            } else {
                // Fall back to config, then default
                parsed
                    .config
                    .get("mint")
                    .filter(|v| !v.is_empty())
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_MINT.to_string())
            };

            match generate_pay_url(
                &parsed.recipient,
                parsed.amount,
                &mint,
                &parsed.memo,
                &parsed.reference,
            ) {
                Ok(url) => {
                    emit(
                        PluginAction::Complete,
                        PluginOutcome::Success,
                        "generated solana pay url",
                        None,
                    );
                    Ok(ToolResult {
                        success: true,
                        output: url,
                        error: None,
                    })
                }
                Err(msg) => {
                    emit(
                        PluginAction::Fail,
                        PluginOutcome::Failure,
                        &msg,
                        None,
                    );
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(msg),
                    })
                }
            }
        }
    }

    fn emit(
        action: PluginAction,
        outcome: PluginOutcome,
        message: &str,
        _extra: Option<String>,
    ) {
        log_record(
            LogLevel::Info,
            &PluginEvent {
                function_name: "solana_pay_request::tool::execute".to_string(),
                action,
                outcome: Some(outcome),
                duration_ms: None,
                attrs: None,
                message: message.to_string(),
            },
        );
    }

    export!(SolanaPayRequest);
}
