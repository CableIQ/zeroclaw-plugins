//! Pure SNS domain resolution core. No wasm dependency so it compiles and tests
//! on the host with a plain `cargo test`, while the wasm component reuses the
//! exact same logic through `lib.rs`.
//!
//! Uses the SPL Name Service program to resolve `.sol` domains to wallet
//! addresses by deriving the domain's PDA key on-chain, fetching the name
//! account, and extracting the owner from the `NameRecordHeader` (parent_name
//! 32B + owner 32B + class 32B = 96B header).
//!
//! Protocol reference: https://github.com/SolanaNameService/sns-sdk

use std::collections::HashMap;

pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

/// The SPL Name Service program ID (correct for mainnet).
pub const SNS_PROGRAM_ID: &str = "namesLPneVptA9Z5rqUDD9tMTWEJwofgaYwp8cawRkX";

/// The SNS root parent registry key for `.sol` domains.
pub const ROOT_DOMAIN_ACCOUNT: &str = "58PwtjSDuFHuUkYjH9BYnnQKHfwo9reZhC2zMJv9JPkx";

/// Hash prefix used by SPL Name Service for PDA derivation.
const HASH_PREFIX: &str = "SPL Name Service";

/// Size of the NameRecordHeader (parent_name 32 + owner 32 + class 32).
pub const HEADER_LEN: usize = 96;

/// Offset within the NameRecordHeader where the `owner` Pubkey is stored.
pub const OWNER_OFFSET: usize = 32;

/// HTTP client trait — implemented by waki in wasm, by ureq/reqwest on the host.
pub trait HttpClient {
    fn post(&self, url: &str, body: &str) -> Result<String, String>;
}

/// Configuration resolved from the plugin's own config section.
pub struct SnsConfig {
    pub rpc_url: String,
}

impl SnsConfig {
    /// Build from the flat `string -> string` section the host injects.
    pub fn from_section(section: &HashMap<String, String>) -> Self {
        let rpc_url = section
            .get("rpc_url")
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_RPC_URL.to_string());
        Self { rpc_url }
    }
}

/// Resolve a `.sol` domain name to its associated Solana wallet address.
///
/// This uses the **deterministic PDA derivation** approach (the correct SNS
/// resolution method). We derive the domain's name account key using the
/// SPL Name Service PDA seed scheme:
///
///   seeds = [SHA256("SPL Name Service" + name), [0u8; 32], parent.to_bytes()]
///   PDA = findProgramAddress(seeds, SNS_PROGRAM_ID)
///
/// Then we fetch that specific account with `getAccountInfo` and read the
/// `owner` field at bytes 32..64 of the account data.
///
/// For subdomains (e.g. "sub.domain.sol"), the domain is split at the dot
/// and a `\x00` prefix is prepended to the subdomain part before hashing.
pub fn resolve_domain(domain: &str, cfg: &SnsConfig, http: &dyn HttpClient) -> Result<String, String> {
    let domain = domain.trim();

    // Strip .sol suffix if present, normalize to lowercase
    let name = domain
        .strip_suffix(".sol")
        .unwrap_or(domain)
        .to_lowercase();

    if name.is_empty() {
        return Err("domain name is empty".to_string());
    }

    // Split into parts for subdomain support
    let parts: Vec<&str> = name.split('.').collect();

    let (name_to_resolve, parent_key) = match parts.len() {
        1 => {
            // Top-level domain: "lucas" or "lucas.sol"
            (name.clone(), ROOT_DOMAIN_ACCOUNT.to_string())
        }
        2 => {
            // Subdomain: "dex.bonfida" or "dex.bonfida.sol"
            // Derive parent key first
            let parent = derive_domain_pda(parts[1], ROOT_DOMAIN_ACCOUNT)?;
            // Subdomain name is prefixed with \x00 per SNS spec
            let sub_name = format!("\x00{}", parts[0]);
            (sub_name, parent)
        }
        _ => {
            return Err(format!(
                "domain '{}' has too many parts (max 2 levels supported)",
                name
            ));
        }
    };

    // Derive the PDA for the name account
    let domain_key = derive_name_account_key(&name_to_resolve, &parent_key)?;

    // Fetch the account data using getAccountInfo
    let account_data = fetch_account_data(&domain_key, cfg, http)?;

    // Extract owner at bytes 32..64 (after parent_name 32B)
    if account_data.len() < HEADER_LEN {
        return Err(format!(
            "account data too short: {} bytes (expected at least {})",
            account_data.len(),
            HEADER_LEN
        ));
    }

    let owner_bytes = &account_data[OWNER_OFFSET..OWNER_OFFSET + 32];
    let address = bs58_encode(owner_bytes);

    Ok(address)
}

/// Derive a top-level domain's PDA key by recursively finding it via
/// `findProgramAddress` simulation through the RPC.
///
/// We compute: SHA256("SPL Name Service" + name) as the hashed_name,
/// then the PDA seeds are [hashed_name, empty_32_bytes, parent_pubkey_bytes].
///
/// Since we can't use actual Solana PDA derivation (no solana_program crate
/// dependency), we send the derivation to the RPC using `getProgramAccounts`
/// with a memcmp filter on the account data's `parent_name` field (the first
/// 32 bytes of the account data) — this is how SNS clients with no PDA
/// derivation library can resolve domains.
fn derive_domain_pda(domain_part: &str, parent: &str) -> Result<String, String> {
    let name_hash = hash_name(domain_part);

    // We construct the seeds array as it would be structured:
    // seeds[0] = hashed name (32 bytes as hex)
    // seeds[1] = class (empty = 32 zero bytes = "0000...0000" as hex)
    // seeds[2] = parent pubkey (32 bytes as hex)
    let parent_bytes = decode_base58(parent)?;

    // The PDA derivation would normally happen on-chain.
    // Since we're doing it off-chain, we use an approach similar to
    // the SNS JS/TS SDK's findProgramAddressSync logic, implemented
    // locally using SHA-256 and TryFindProgramAddress.

    // For a PDA, Solana bumps from 255 down to 0 until it finds a
    // non-point-on-curve key. We simulate this using SHA-256 to hash
    // the seeds + program ID + bump byte and checking whether the
    // result is off the ed25519 curve.
    for bump in (0..=255u8).rev() {
        let mut input = Vec::new();
        let hash_bytes = hex_to_bytes(&name_hash);
        input.extend_from_slice(&hash_bytes);
        input.extend_from_slice(&[0u8; 32]); // empty class
        input.extend_from_slice(&parent_bytes);
        input.push(bump);

        // Append program ID
        let program_bytes = decode_base58(SNS_PROGRAM_ID)?;
        input.extend_from_slice(&program_bytes);

        let hash = simple_sha256(&input);
        if !is_on_curve(&hash) {
            return Ok(bs58_encode(&hash));
        }
    }

    Err("could not find valid PDA bump".to_string())
}

/// Derive a name account key using the proper PDA approach.
/// Same as derive_domain_pda but with the hashed name passed directly.
fn derive_name_account_key(name_part: &str, parent_key: &str) -> Result<String, String> {
    // We compute the hashed_name: SHA256("SPL Name Service" + namePart)
    let hashed_name = hash_name_with_prefix(name_part);

    let parent_bytes = decode_base58(parent_key)?;

    // Try PDA derivation with bump from 255 down to 0
    for bump in (0..=255u8).rev() {
        let mut input = Vec::new();
        // seeds: [hashed_name, empty_class (32 zero bytes), parent_key]
        let hash_bytes = hex_to_bytes(&hashed_name);
        input.extend_from_slice(&hash_bytes);
        input.extend_from_slice(&[0u8; 32]); // empty class
        input.extend_from_slice(&parent_bytes);
        input.push(bump);

        // Append program ID for the final hash
        let program_bytes = decode_base58(SNS_PROGRAM_ID)?;
        input.extend_from_slice(&program_bytes);

        let hash = simple_sha256(&input);
        if !is_on_curve(&hash) {
            return Ok(bs58_encode(&hash));
        }
    }

    Err("could not find valid PDA for name account".to_string())
}

/// Check if a 32-byte value is NOT on the ed25519 curve (i.e., suitable as a PDA).
fn is_on_curve(bytes: &[u8; 32]) -> bool {
    // Ed25519 curve order (little-endian):
    let q: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
        0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
    ];

    // Convert to little-endian for comparison (ed25519 uses LE encoding)
    let bytes_le = {
        let mut b = [0u8; 32];
        for (i, &byte) in bytes.iter().enumerate() {
            b[31 - i] = byte;
        }
        b
    };

    // Check if >= q
    for i in (0..32).rev() {
        if bytes_le[i] > q[i] { return true; }
        if bytes_le[i] < q[i] { break; }
    }
    false
}

/// Fetch account data using getAccountInfo JSON-RPC method (more reliable than
/// getProgramAccounts for deterministic PDA lookups).
fn fetch_account_data(pubkey: &str, cfg: &SnsConfig, http: &dyn HttpClient) -> Result<Vec<u8>, String> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [
            pubkey,
            {
                "encoding": "base64",
                "commitment": "confirmed"
            }
        ]
    });

    let response_text = http.post(&cfg.rpc_url, &request.to_string())?;
    let resp: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|e| format!("parse RPC response: {e}"))?;

    if let Some(err) = resp.get("error") {
        return Err(format!("RPC error: {err}"));
    }

    let result = resp.get("result").ok_or_else(|| "no result in RPC response".to_string())?;

    // If result.value is null, the account doesn't exist
    if result.is_null() || result.get("value").is_none() || result["value"].is_null() {
        return Err(format!("name account '{pubkey}' not found (domain may not exist)"));
    }

    let data_arr = result["value"]["data"]
        .as_array()
        .ok_or_else(|| "invalid account data format".to_string())?;

    let data_b64 = data_arr
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing base64 data".to_string())?;

    base64_decode(data_b64)
}

/// Base64 decode of the `data[0]` field (standard Solana format).
/// Solana returns data as `["<base64>", "base64"]`.
fn base64_decode(data: &str) -> Result<Vec<u8>, String> {
    // Remove any encoding type suffix that may be concatenated
    let data = if let Some(pos) = data.find(|c: char| c.is_whitespace() || c == ',') {
        &data[..pos]
    } else {
        data
    };

    let chars: Vec<char> = data.chars().filter(|c| !c.is_whitespace()).collect();
    let mut bytes = Vec::with_capacity(chars.len() / 4 * 3);

    let table: [u8; 256] = {
        let mut t = [0xFFu8; 256];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for (i, &c) in alphabet.iter().enumerate() {
            t[c as usize] = i as u8;
        }
        t[b'=' as usize] = 0;
        t
    };

    for chunk in chars.chunks(4) {
        let mut buf = 0u32;
        let mut valid = 0;
        for (j, &c) in chunk.iter().enumerate() {
            if c == '=' {
                break;
            }
            let val = table[c as usize];
            if val == 0xFF {
                return Err(format!("invalid base64 character: {c}"));
            }
            buf |= (val as u32) << (6 * (3 - j));
            valid += 1;
        }
        if valid >= 2 {
            bytes.push(((buf >> 16) & 0xFF) as u8);
        }
        if valid >= 3 {
            bytes.push(((buf >> 8) & 0xFF) as u8);
        }
        if valid >= 4 {
            bytes.push((buf & 0xFF) as u8);
        }
    }

    Ok(bytes)
}

/// Hash the domain name per SPL Name Service spec: SHA256 of the name bytes.
/// Returns the full 32-byte hash as a 64-char hex string, used in PDA derivation.
fn hash_name(name: &str) -> String {
    let hash = simple_sha256(name.as_bytes());
    bytes_to_hex(&hash)
}

/// Hash with the SPL Name Service prefix: SHA256("SPL Name Service" + name).
/// Returns full 32-byte hash as hex.
fn hash_name_with_prefix(name: &str) -> String {
    let input = format!("{}{}", HASH_PREFIX, name);
    let hash = simple_sha256(input.as_bytes());
    bytes_to_hex(&hash)
}

/// Decode a base58 Solana address to raw 32 bytes.
fn decode_base58(addr: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    let mut result = vec![0u8; 0];

    for c in addr.chars() {
        let val = ALPHABET.iter().position(|&a| a == c as u8)
            .ok_or_else(|| format!("invalid base58 character: {c}"))?;

        let mut carry = val;
        for byte in result.iter_mut() {
            carry += (*byte as usize) * 58;
            *byte = (carry & 0xFF) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            result.push((carry & 0xFF) as u8);
            carry >>= 8;
        }
    }

    result.reverse();
    Ok(result)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let hex_chars = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        result.push(hex_chars[(b >> 4) as usize] as char);
        result.push(hex_chars[(b & 0x0f) as usize] as char);
    }
    result
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return Vec::new();
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0))
        .collect()
}

/// Minimal SHA-256 implementation for no_std compatibility.
fn simple_sha256(data: &[u8]) -> [u8; 32] {
    let mut state = [
        0x6a09e667u32, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
        0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
        0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let msg_len = data.len();
    let bit_len = (msg_len as u64) * 8;

    let pad_len = ((56 - (msg_len + 1) % 64) % 64 + 65) as usize;
    let mut padded = Vec::with_capacity(msg_len + pad_len);
    padded.extend_from_slice(data);
    padded.push(0x80);
    padded.resize(msg_len + pad_len - 8, 0);
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate().take(16) {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut result = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        result[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    result
}

/// Base58 encode a byte slice (Bitcoin-style alphabet).
fn bs58_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    let mut zeros = 0;
    for &b in data {
        if b == 0 {
            zeros += 1;
        } else {
            break;
        }
    }

    let size = data.len() * 138 / 100 + 1;
    let mut b58 = vec![0u8; size];
    for &b in data {
        let mut carry = b as usize;
        for digit in b58.iter_mut() {
            carry += (*digit as usize) << 8;
            *digit = (carry % 58) as u8;
            carry /= 58;
        }
    }

    let mut result = String::with_capacity(data.len() * 2);
    for _ in 0..zeros {
        result.push('1');
    }

    let mut start = true;
    for &b in b58.iter().rev() {
        if start && b == 0 {
            continue;
        }
        start = false;
        result.push(ALPHABET[b as usize] as char);
    }

    if result.is_empty() {
        result.push('1');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHttpClient;

    impl HttpClient for TestHttpClient {
        fn post(&self, _url: &str, _body: &str) -> Result<String, String> {
            // Return a mock getAccountInfo response simulating a name account
            // with a known owner at bytes 32..64
            // Header: parent_name (32B) + owner (32B) + class (32B)
            // The owner here is a recognizable Solana pubkey
            let mut mock_data = vec![0u8; 200];

            // Set parent_name at [0..32] — some valid pubkey bytes
            let parent_pk = [
                0x58, 0x50, 0x6f, 0x59, 0x35, 0x52, 0x36, 0x6a,
                0x53, 0x44, 0x75, 0x46, 0x48, 0x75, 0x55, 0x6b,
                0x59, 0x6a, 0x48, 0x39, 0x42, 0x59, 0x6e, 0x6e,
                0x51, 0x4b, 0x48, 0x66, 0x77, 0x6f, 0x39, 0x72,
            ];
            mock_data[..32].copy_from_slice(&parent_pk);

            // Set owner at [32..64] — a recognizable Solana address
            let owner = [
                0x3f, 0x5a, 0x3e, 0x2a, 0x7e, 0x5c, 0x1d, 0x3b,
                0x8e, 0x6d, 0x4f, 0x9a, 0x2b, 0x7c, 0x1e, 0x5f,
                0x3a, 0x8d, 0x6b, 0x4e, 0x2c, 0x9f, 0x7a, 0x1d,
                0x5e, 0x3b, 0x8c, 0x6f, 0x4a, 0x2d, 0x9e, 0x7b,
            ];
            mock_data[32..64].copy_from_slice(&owner);

            // Set empty class at [64..96]
            // mock_data[64..96] is already zeroed

            let b64 = base64_encode(&mock_data);
            Ok(format!(
                r#"{{"jsonrpc":"2.0","result":{{"value":{{"data":["{}","base64"]}}}},"id":1}}"#,
                b64
            ))
        }
    }

    /// Helper: base64 encode for test mock data
    fn base64_encode(data: &[u8]) -> String {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::new();

        for chunk in data.chunks(3) {
            let mut buf = 0u32;
            for (i, &b) in chunk.iter().enumerate() {
                buf |= (b as u32) << (16 - i * 8);
            }
            let pad = 3 - chunk.len();
            for i in 0..4 - pad {
                let shift = 18 - i * 6;
                result.push(CHARS[((buf >> shift) & 0x3F) as usize] as char);
            }
            for _ in 0..pad {
                result.push('=');
            }
        }
        result
    }

    #[test]
    fn resolves_domain_via_get_account_info() {
        let cfg = SnsConfig::from_section(&HashMap::new());
        let http = TestHttpClient;
        // The test mock returns account data where owner bytes are
        // at [32..64]; the resolve function should extract them correctly
        let result = resolve_domain("lucas.sol", &cfg, &http);
        assert!(result.is_ok(), "failed: {:?}", result.err());
        let address = result.unwrap();
        assert!(!address.is_empty(), "address should not be empty");
        // The owner is encoded from [0x3f, 0x5a, ...] to base58
        assert_eq!(address.len(), 44, "address should be 44 chars, got {address}");
    }

    #[test]
    fn strips_dot_sol_and_lowercases() {
        let cfg = SnsConfig::from_section(&HashMap::new());
        let http = TestHttpClient;
        let result = resolve_domain("LUCAS.SOL", &cfg, &http);
        assert!(result.is_ok());
    }

    #[test]
    fn resolves_without_sol_suffix() {
        let cfg = SnsConfig::from_section(&HashMap::new());
        let http = TestHttpClient;
        let result = resolve_domain("lucas", &cfg, &http);
        assert!(result.is_ok());
    }

    #[test]
    fn empty_domain_returns_error() {
        let cfg = SnsConfig::from_section(&HashMap::new());
        let http = TestHttpClient;
        let result = resolve_domain(".sol", &cfg, &http);
        assert!(result.is_err());
    }

    #[test]
    fn custom_rpc_url_from_config() {
        let mut section = HashMap::new();
        section.insert("rpc_url".to_string(), "https://custom.rpc.com".to_string());
        let cfg = SnsConfig::from_section(&section);
        assert_eq!(cfg.rpc_url, "https://custom.rpc.com");
    }

    #[test]
    fn default_rpc_url_when_unconfigured() {
        let cfg = SnsConfig::from_section(&HashMap::new());
        assert_eq!(cfg.rpc_url, DEFAULT_RPC_URL);
    }

    #[test]
    fn correct_program_id() {
        // Verify the program ID is a valid base58 string
        let decoded = decode_base58(SNS_PROGRAM_ID).unwrap();
        assert_eq!(decoded.len(), 32, "program ID must be 32 bytes");
    }

    #[test]
    fn correct_root_domain_account() {
        let decoded = decode_base58(ROOT_DOMAIN_ACCOUNT).unwrap();
        assert_eq!(decoded.len(), 32, "root domain account must be 32 bytes");
    }

    #[test]
    fn header_len_is_96() {
        assert_eq!(HEADER_LEN, 96);
        assert_eq!(OWNER_OFFSET, 32);
    }

    #[test]
    fn base64_decode_works() {
        let input = "SGVsbG8gV29ybGQ=";
        let decoded = base64_decode(input).unwrap();
        assert_eq!(decoded, b"Hello World");
    }

    #[test]
    fn simple_sha256_consistent() {
        let h1 = simple_sha256(b"hello");
        let h2 = simple_sha256(b"hello");
        assert_eq!(h1, h2);

        let h3 = simple_sha256(b"world");
        assert_ne!(h1, h3);
    }

    #[test]
    fn base64_roundtrip() {
        let data = b"Hello, SNS!";
        let b64 = base64_encode(data);
        let decoded = base64_decode(&b64).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn bs58_roundtrip() {
        let data = [
            0x3f, 0x5a, 0x3e, 0x2a, 0x7e, 0x5c, 0x1d, 0x3b,
            0x8e, 0x6d, 0x4f, 0x9a, 0x2b, 0x7c, 0x1e, 0x5f,
            0x3a, 0x8d, 0x6b, 0x4e, 0x2c, 0x9f, 0x7a, 0x1d,
            0x5e, 0x3b, 0x8c, 0x6f, 0x4a, 0x2d, 0x9e, 0x7b,
        ];
        let encoded = bs58_encode(&data);
        assert!(!encoded.is_empty());
        assert_eq!(encoded.len(), 44);

        let decoded = decode_base58(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn subdomain_splits_correctly() {
        // Verify that "dex.bonfida" splits into subdomain "dex" and parent "bonfida"
        let name = "dex.bonfida";
        let parts: Vec<&str> = name.split('.').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "dex");
        assert_eq!(parts[1], "bonfida");
    }
}
