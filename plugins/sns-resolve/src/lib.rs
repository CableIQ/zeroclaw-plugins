//! A ZeroClaw WIT tool plugin: `sns-resolve`.
//!
//! Resolves `.sol` domain names to Solana wallet addresses by querying the
//! SNS (Solana Name Service) on-chain program.
//!
//! The pure resolution core lives in [`resolve`] with no wasm dependency, so it
//! compiles and tests on the host with a plain `cargo test`; the wasm component
//! reuses the exact same logic through this shim.
//!
//! Build:  rustup target add wasm32-wasip2
//!         cargo build --target wasm32-wasip2 --release

pub mod resolve;

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use std::collections::HashMap;

    use crate::resolve::{resolve_domain, SnsConfig};
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::logging::{
        log_record, LogLevel, PluginAction, PluginEvent, PluginOutcome,
    };

    use solana_client_wasip2::RpcClient;

    struct SnsResolve;

    const PLUGIN_NAME: &str = "sns-resolve";
    const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
    const TOOL_NAME: &str = "sns-resolve";

    #[derive(serde::Deserialize)]
    struct ExecuteArgs {
        domain: String,
        #[serde(rename = "__config", default)]
        config: HashMap<String, String>,
    }

    impl PluginInfo for SnsResolve {
        fn plugin_name() -> String {
            PLUGIN_NAME.to_string()
        }

        fn plugin_version() -> String {
            PLUGIN_VERSION.to_string()
        }
    }

    impl Tool for SnsResolve {
        fn name() -> String {
            TOOL_NAME.to_string()
        }

        fn description() -> String {
            "Resolve a .sol domain name to its Solana wallet address. \
             Given a domain like 'lucas.sol', queries the SNS on-chain program \
             and returns the associated wallet address."
                .to_string()
        }

        fn parameters_schema() -> String {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "domain": {
                        "type": "string",
                        "description": "The .sol domain name to resolve, e.g. 'lucas.sol'."
                    }
                },
                "required": ["domain"]
            })
            .to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let parsed: ExecuteArgs = match serde_json::from_str(&args) {
                Ok(a) => a,
                Err(e) => {
                    emit(PluginAction::Fail, PluginOutcome::Failure, "invalid arguments", None);
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
                .unwrap_or_else(|| crate::resolve::DEFAULT_RPC_URL.to_string());

            let client = RpcClient::new(&rpc_url);

            match resolve_domain(&parsed.domain, &client) {
                Ok(address) => {
                    emit(PluginAction::Complete, PluginOutcome::Success, "resolved domain", None);
                    Ok(ToolResult { success: true, output: address, error: None })
                }
                Err(e) => {
                    emit(PluginAction::Fail, PluginOutcome::Failure, "resolution failed", None);
                    Ok(ToolResult { success: false, output: String::new(), error: Some(e) })
                }
            }
        }
    }

    fn emit(action: PluginAction, outcome: PluginOutcome, message: &str, _extra: Option<usize>) {
        log_record(
            LogLevel::Info,
            &PluginEvent {
                function_name: "sns_resolve::tool::execute".to_string(),
                action,
                outcome: Some(outcome),
                duration_ms: None,
                attrs: None,
                message: message.to_string(),
            },
        );
    }

    export!(SnsResolve);
}
