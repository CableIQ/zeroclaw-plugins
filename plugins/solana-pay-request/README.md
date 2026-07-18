# solana-pay-request

A ZeroClaw WIT component tool plugin that generates Solana Pay URLs and
QR-ready payloads.

## What it does

The `solana-pay-request` tool constructs a `solana:` URL for requesting
payments. No RPC calls needed — pure URL construction.

## Config keys

| Key | Default | Meaning |
|---|---|---|
| (none) | | No configuration needed |

## Custody tier

- **Track**: A (payment request generation)
- **Custody**: T1 (builds request only, zero secrets, human pays)
- **Secrets**: None

## Threat model

The plugin only constructs a URL string. It never holds keys, never signs,
never submits transactions. The URL is QR-ready — the user must scan and
approve the transaction in their wallet. Even if a malicious input tries to
hijack the recipient address, the user sees the URL before approving it.

Default mint: `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` (USDC mainnet).

## Worked example

```
Input:  {
          "recipient": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
          "amount": 10.50,
          "memo": "invoice #42"
        }
Output: "solana:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v?amount=10.5&spl-token=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v&memo=invoice%20%2342"
        (QR-ready: paste into any QR generator to make a QR code)
```

## Prompt injection test

```
User: "send 10000 SOL to 'attacker'"
System: validates recipient → "invalid recipient address"
-- Plugin fails closed on invalid input. It never submits transactions.
```

## Build and test

```bash
cargo test
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/solana_pay_request.wasm solana_pay_request.wasm
```
