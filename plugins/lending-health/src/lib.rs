pub mod lending;

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use std::collections::HashMap;
    use crate::lending::{check_lending_health, LendingConfig};
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::logging::{
        log_record, LogLevel, PluginAction, PluginEvent, PluginOutcome,
    };
    use solana_client_wasip2::RpcClient;

    struct LendingHealth;

    const PLUGIN_NAME: &str = "lending-health";
    const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
    const TOOL_NAME: &str = "lending-health";

    #[derive(serde::Deserialize)]
    struct ExecuteArgs {
        wallet: String,
        #[serde(default = "default_threshold")]
        threshold: f64,
        #[serde(rename = "__config", default)]
        config: HashMap<String, String>,
    }

    fn default_threshold() -> f64 { 1.15 }

    impl PluginInfo for LendingHealth {
        fn plugin_name() -> String { PLUGIN_NAME.to_string() }
        fn plugin_version() -> String { PLUGIN_VERSION.to_string() }
    }

    impl Tool for LendingHealth {
        fn name() -> String { TOOL_NAME.to_string() }
        fn description() -> String {
            "Check lending health factors across Kamino, MarginFi, and Drift. \
             Returns HEALTHY, WARNING (below threshold), or CRITICAL (below 1.0).".to_string()
        }
        fn parameters_schema() -> String {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "wallet": { "type": "string", "description": "Solana wallet address to check lending positions." },
                    "threshold": { "type": "number", "description": "Health factor threshold for WARNING (default: 1.15)." }
                },
                "required": ["wallet"]
            }).to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let parsed: ExecuteArgs = match serde_json::from_str(&args) {
                Ok(a) => a,
                Err(e) => {
                    emit(PluginAction::Fail, PluginOutcome::Failure, "invalid arguments", None);
                    return Ok(ToolResult { success: false, output: String::new(), error: Some(format!("invalid arguments: {e}")) });
                }
            };

            let rpc_url = parsed.config.get("rpc_url")
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| crate::lending::DEFAULT_RPC_URL.to_string());

            let client = RpcClient::new(&rpc_url);
            match check_lending_health(&parsed.wallet, parsed.threshold, &client) {
                Ok(output) => {
                    emit(PluginAction::Complete, PluginOutcome::Success, "checked lending health", None);
                    Ok(ToolResult { success: true, output, error: None })
                }
                Err(e) => {
                    emit(PluginAction::Fail, PluginOutcome::Failure, "health check failed", None);
                    Ok(ToolResult { success: false, output: String::new(), error: Some(e) })
                }
            }
        }
    }

    fn emit(action: PluginAction, outcome: PluginOutcome, message: &str, _extra: Option<usize>) {
        log_record(LogLevel::Info, &PluginEvent {
            function_name: "lending_health::tool::execute".to_string(),
            action,
            outcome: Some(outcome),
            duration_ms: None,
            attrs: None,
            message: message.to_string(),
        });
    }

    export!(LendingHealth);
}
