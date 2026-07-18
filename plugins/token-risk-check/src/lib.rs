//! A ZeroClaw WIT tool plugin: `token-risk-check`.
//!
//! Given a Solana SPL token mint address, queries on-chain data and returns a
//! risk assessment: RED (dangerous), AMBER (caution), or GREEN (safe).
//!
//! Checks performed: mint authority, freeze authority, holder concentration
//! (top 10 holders %), Token-2022 extensions, transfer hooks, transfer fees,
//! permanent delegate.
//!
//! Build:  rustup target add wasm32-wasip2
//!         cargo build --target wasm32-wasip2 --release

pub mod risk;

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use std::collections::HashMap;

    use crate::risk::{assess_token, HttpClient, RiskConfig};
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::logging::{
        log_record, LogLevel, PluginAction, PluginEvent, PluginOutcome,
    };

    struct TokenRiskCheck;

    const PLUGIN_NAME: &str = "token-risk-check";
    const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
    const TOOL_NAME: &str = "token-risk-check";

    #[derive(serde::Deserialize)]
    struct ExecuteArgs {
        mint: String,
        #[serde(rename = "__config", default)]
        config: HashMap<String, String>,
    }

    impl PluginInfo for TokenRiskCheck {
        fn plugin_name() -> String { PLUGIN_NAME.to_string() }
        fn plugin_version() -> String { PLUGIN_VERSION.to_string() }
    }

    impl Tool for TokenRiskCheck {
        fn name() -> String { TOOL_NAME.to_string() }

        fn description() -> String {
            "Assess risk of a Solana SPL token by mint address. \
             Checks mint authority, freeze authority, holder concentration, \
             Token-2022 extensions, transfer hooks, transfer fees, and \
             permanent delegate. Returns RED/AMBER/GREEN rating with specific reasons."
                .to_string()
        }

        fn parameters_schema() -> String {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "mint": {
                        "type": "string",
                        "description": "The Solana SPL token mint address to assess."
                    }
                },
                "required": ["mint"]
            })
            .to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let parsed: ExecuteArgs = match serde_json::from_str(&args) {
                Ok(a) => a,
                Err(e) => {
                    emit(PluginAction::Fail, PluginOutcome::Failure, "invalid arguments", None);
                    return Ok(ToolResult { success: false, output: String::new(), error: Some(format!("invalid arguments: {e}")) });
                }
            };

            let cfg = RiskConfig::from_section(&parsed.config);
            let http = WakiHttpClient;
            match assess_token(&parsed.mint, &cfg, &http) {
                Ok(assessment) => {
                    emit(PluginAction::Complete, PluginOutcome::Success, "token assessed", None);
                    Ok(ToolResult { success: true, output: assessment, error: None })
                }
                Err(e) => {
                    emit(PluginAction::Fail, PluginOutcome::Failure, "assessment failed", None);
                    Ok(ToolResult { success: false, output: String::new(), error: Some(e) })
                }
            }
        }
    }

    struct WakiHttpClient;

    impl HttpClient for WakiHttpClient {
        fn post(&self, url: &str, body: &str) -> Result<String, String> {
            let body_val: serde_json::Value = serde_json::from_str(body)
                .map_err(|e| format!("invalid JSON body: {e}"))?;

            let resp = waki::Client::new()
                .post(url)
                .header("Content-Type", "application/json")
                .json(&body_val)
                .send()
                .map_err(|e| format!("http request failed: {e}"))?;

            let status = resp.status_code();
            let val: serde_json::Value = resp.json().map_err(|e| format!("read body: {e}"))?;

            if status != 200 {
                return Err(format!("RPC error (status {status})"));
            }
            Ok(val.to_string())
        }
    }

    fn emit(action: PluginAction, outcome: PluginOutcome, message: &str, _extra: Option<usize>) {
        log_record(LogLevel::Info, &PluginEvent {
            function_name: "token_risk_check::tool::execute".to_string(),
            action,
            outcome: Some(outcome),
            duration_ms: None,
            attrs: None,
            message: message.to_string(),
        });
    }

    export!(TokenRiskCheck);
}
