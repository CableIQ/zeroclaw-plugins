//! Pure Solana Pay URL construction core. No wit-bindgen or wasm dependency so
//! it compiles and tests on the host with a plain `cargo test`, while the wasm
//! component reuses the exact same logic through `lib.rs`.
//!
//! Builds a [`solana:`] URI per the
//! [Solana Pay spec](https://docs.solanapay.com/core/transfer-request).
//!
//! Format:
//! ```text
//! solana:<recipient>?amount=<amount>[&spl-token=<mint>][&memo=<memo>][&reference=<reference>]
//! ```

/// Default USDC mint on Solana mainnet.
pub const DEFAULT_USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Placeholder config struct, kept for consistency with the host's `__config`
/// injection pattern. All fields can be overridden per-call.
#[derive(Debug, Default, Clone)]
pub struct PayConfig {
    /// Override default SPL token mint.
    pub mint: Option<String>,
}

/// Generate a Solana Pay transfer request URL.
///
/// # Arguments
///
/// * `recipient` - The recipient's Solana address (base58). Must be non-empty.
/// * `amount` - Transfer amount in token decimal units (e.g. `1.50` for 1.50 USDC).
///   Must be greater than zero.
/// * `mint` - SPL token mint address. Pass `DEFAULT_USDC_MINT` or an empty string
///   to use the default USDC mint.
/// * `memo` - Optional memo text to include in the transaction.
/// * `reference` - Optional reference tag (base58-encoded 32-byte pubkey) for
///   payment tracking.
///
/// # Returns
///
/// A Solana Pay URI string on success, or an error description on failure. The
/// returned URL is QR-code-ready: any Solana Pay compatible wallet can scan it
/// to initiate the transfer.
///
/// # Errors
///
/// * If `recipient` is empty.
/// * If `amount` is zero or negative.
pub fn generate_pay_url(
    recipient: &str,
    amount: f64,
    mint: &str,
    memo: &Option<String>,
    reference: &Option<String>,
) -> Result<String, String> {
    let recipient = recipient.trim();
    if recipient.is_empty() {
        return Err("recipient address must not be empty".to_string());
    }

    if amount <= 0.0 {
        return Err("amount must be greater than zero".to_string());
    }

    let mint = if mint.is_empty() {
        DEFAULT_USDC_MINT
    } else {
        mint
    };

    let mut url = format!("solana:{recipient}?amount={amount}&spl-token={mint}");

    if let Some(m) = memo {
        let encoded = urlencode(m);
        url.push_str(&format!("&memo={encoded}"));
    }

    if let Some(r) = reference {
        let encoded = urlencode(r);
        url.push_str(&format!("&reference={encoded}"));
    }

    Ok(url)
}

/// Minimal URL percent-encoding for query parameter values.
/// Encodes spaces as `%20`, plus as `%2B`, and a handful of other reserved
/// characters. Keeps alphanumeric characters, hyphen, underscore, period,
/// and tilde unencoded (per RFC 3986 'unreserved' set).
fn urlencode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push_str("%20"),
            _ => {
                result.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_usdc_url() {
        let url = generate_pay_url(
            "5PYKNqzVzB4r4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx",
            1.50,
            DEFAULT_USDC_MINT,
            &None,
            &None,
        )
        .unwrap();

        assert!(url.starts_with("solana:"));
        assert!(url.contains("5PYKNqzVzB4r4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx"));
        assert!(url.contains("amount=1.5"));
        assert!(url.contains("spl-token=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"));
        assert!(!url.contains("memo="));
        assert!(!url.contains("reference="));
    }

    #[test]
    fn test_with_memo_and_reference() {
        let url = generate_pay_url(
            "5PYKNqzVzB4r4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx",
            0.01,
            DEFAULT_USDC_MINT,
            &Some("invoice #123".to_string()),
            &Some("4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx5PYKNqzVzB4r4".to_string()),
        )
        .unwrap();

        assert!(url.starts_with("solana:"));
        assert!(url.contains("memo=invoice%20%23123"));
        assert!(url.contains("reference=4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx5PYKNqzVzB4r4"));
        assert!(url.contains("amount=0.01"));
    }

    #[test]
    fn test_custom_mint() {
        let custom_mint = "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R";
        let url = generate_pay_url(
            "RecipientAddressHere11111111111111111111111111111",
            5.0,
            custom_mint,
            &None,
            &None,
        )
        .unwrap();

        assert!(url.contains(&format!("spl-token={custom_mint}")));
    }

    #[test]
    fn test_empty_recipient_fails() {
        let result = generate_pay_url("   ", 1.0, DEFAULT_USDC_MINT, &None, &None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("recipient address must not be empty"));
    }

    #[test]
    fn test_zero_amount_fails() {
        let result = generate_pay_url(
            "5PYKNqzVzB4r4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx",
            0.0,
            DEFAULT_USDC_MINT,
            &None,
            &None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("amount must be greater than zero"));
    }

    #[test]
    fn test_negative_amount_fails() {
        let result = generate_pay_url(
            "5PYKNqzVzB4r4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx",
            -5.0,
            DEFAULT_USDC_MINT,
            &None,
            &None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_mint_uses_default() {
        let url = generate_pay_url(
            "5PYKNqzVzB4r4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx",
            2.0,
            "",
            &None,
            &None,
        )
        .unwrap();

        assert!(url.contains(&format!("spl-token={DEFAULT_USDC_MINT}")));
    }

    #[test]
    fn test_urlencode_memo_spaces_and_special_chars() {
        let url = generate_pay_url(
            "5PYKNqzVzB4r4Q3y2oYhVLBhVgvGZ6s9gJX9KjKxVjKx",
            0.5,
            DEFAULT_USDC_MINT,
            &Some("order: 42 & payment".to_string()),
            &None,
        )
        .unwrap();

        // space -> %20, & -> %26
        assert!(url.contains("memo=order%3A%2042%20%26%20payment"));
    }
}
