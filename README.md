# ZeroClaw Solana WIT Tool Plugins

Four WIT component tool plugins for Solana blockchain operations on ZeroClaw.
Each compiles to `wasm32-wasip2` and follows the ZeroClaw reference plugin
format (see `plugins/redact-text`).

## Plugins

| Plugin | Track | Custody | Network | What it does |
|---|---|---|---|---|
| `sns-resolve` | D | T0 | RPC | Resolve `.sol` names to wallet addresses |
| `token-risk-check` | D | T0 | RPC | Assess SPL token risk (RED/AMBER/GREEN) |
| `wallet-narrate` | D | T0 | RPC | Turn transaction history into English sentences |
| `solana-pay-request` | A | T1 | None | Generate Solana Pay URLs and QR payloads |

## Directory structure

```
plugins/
  sns-resolve/
    Cargo.toml          # cdylib + rlib, wit-bindgen 0.46, waki 0.5.1 (wasm)
    manifest.toml        # name, wasm_path, capabilities, permissions
    src/
      lib.rs            # #[cfg(target_family = "wasm")] component shim
      resolve.rs         # pure SNS resolution core (no wasm deps)
    sns_resolve.wasm     # prebuilt component (340 KB)
    README.md

  token-risk-check/
    Cargo.toml
    manifest.toml
    src/
      lib.rs
      risk.rs            # pure risk assessment core
    token_risk_check.wasm (351 KB)
    README.md

  wallet-narrate/
    Cargo.toml
    manifest.toml
    src/
      lib.rs
      narrate.rs         # pure narration core
    wallet_narrate.wasm (358 KB)
    README.md

  solana-pay-request/
    Cargo.toml           # no waki dependency (no RPC needed)
    manifest.toml        # permissions: config_read only (no network)
    src/
      lib.rs
      pay.rs             # pure URL construction core
    solana_pay_request.wasm (172 KB)
    README.md
```

## Config keys

All RPC-based plugins read from their own jailed config section:

```toml
[plugins.config.sns-resolve]
rpc_url = "https://api.mainnet-beta.solana.com"

[plugins.config.token-risk-check]
rpc_url = "https://api.mainnet-beta.solana.com"

[plugins.config.wallet-narrate]
rpc_url = "https://api.mainnet-beta.solana.com"

# solana-pay-request needs no config
```

## Build requirements

- Rust target: `wasm32-wasip2`
- No `solana-sdk`, no `solana-client` (won't compile to wasm)
- HTTP uses `waki` (blocking `wasi:http`) behind `cfg(target_family = "wasm")`
- Core modules are pure Rust with zero wasm dependency

## Build commands

```bash
for p in sns-resolve token-risk-check wallet-narrate solana-pay-request; do
  cd plugins/$p
  cargo test                           # host tests, no wasm needed
  cargo build --target wasm32-wasip2 --release
  cp target/wasm32-wasip2/release/${p//-/_}.wasm ${p//-/_}.wasm
  cd ../..
done
```

## What fought us on wasm32-wasip2

1. **waki 0.5.1 API mismatch**: `Response` has `.status_code()` not `.status()`,
   and `.json::<T>()` to read the body (no `.text()` method). Use `.json()`
   on the `RequestBuilder` before `.send()` to send JSON bodies.

2. **No `solana-sdk`/`solana-client`**: These crates won't compile to wasm32-wasip2.
   We built pure Rust implementations for SHA-256, base58, and base64 encoding
   to avoid any dependency on Solana SDK crates.

3. **Standalone workspace**: Each plugin must have its own `[workspace]` section
   (they are built as standalone crates, not part of the host workspace).

4. **crate-type = ["cdylib", "rlib"]**: The `cdylib` produces the wasm component;
   `rlib` allows the pure core to be tested on the host.

5. **config_read permission**: Without `config_read` in `manifest.toml`,
   the `__config` field is always an empty map.

6. **Tool name format**: Tool functions use `TOOL_NAME` (kebab-case like
   `sns-resolve`) for the LLM-facing tool name, distinct from the plugin name.

## Worked examples

### 1. SNS Resolve

```
$ zeroclaw tool sns-resolve '{"domain": "lucas.sol"}'
"LUCASxYZ...ABC123"
```

### 2. Token Risk Check

```
$ zeroclaw tool token-risk-check '{"mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"}'
TOKEN RISK: GREEN
Mint: EPjFWdd5...
Decimals: 6
Supply: 1000.00M

OK:
  No mint authority (supply is fixed)
```

### 3. Wallet Narrate

```
$ zeroclaw tool narrate-wallet '{"wallet": "So11111111111111111111111111111111111111112", "limit": 3}'
Sent 0.500000 SOL to ABC...XYZ
Received 100.000000 USDC from DEF...UVW
Swapped 1 SOL for 190 USDC on Jupiter
```

### 4. Solana Pay Request

```
$ zeroclaw tool solana-pay-request '{"recipient": "EPjFWdd...", "amount": 10.50, "memo": "invoice #42"}'
solana:EPjFWdd...?amount=10.5&spl-token=EPjFWdd...&memo=invoice%20%2342
(QR-ready URL)
```
