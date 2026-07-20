//! Pure SNS domain resolution core. No wasm dependency so it compiles and tests
//! on the host with a plain `cargo test`, while the wasm component reuses the
//! exact same logic through `lib.rs`.
//!
//! Uses `solana-client-wasip2::RpcClient` for all RPC calls.

use std::collections::HashMap;
use std::str::FromStr;

use solana_client_wasip2::{
    pubkey::Pubkey,
    rpc::config::RpcAccountInfoConfig,
    RpcClient, RpcTransport,
};

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

/// Configuration resolved from the plugin's own config section.
pub struct SnsConfig {
    pub rpc_url: String,
}

impl SnsConfig {
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
/// Uses `RpcClient` from `solana-client-wasip2` — PDA derivation is done
/// locally, then a single `get_account` call fetches the name account.
pub fn resolve_domain<T: RpcTransport>(
    domain: &str,
    client: &RpcClient<T>,
) -> Result<String, String> {
    let domain = domain.trim();

    let name = domain
        .strip_suffix(".sol")
        .unwrap_or(domain)
        .to_lowercase();

    if name.is_empty() {
        return Err("domain name is empty".to_string());
    }

    let parts: Vec<&str> = name.split('.').collect();

    let (name_to_resolve, parent_key) = match parts.len() {
        1 => (name.clone(), ROOT_DOMAIN_ACCOUNT.to_string()),
        2 => {
            let parent = derive_domain_pda(parts[1], ROOT_DOMAIN_ACCOUNT)?;
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

    // Fetch the account data
    let account_data = fetch_account_data::<T>(&domain_key, client)?;

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

/// Derive a top-level domain's PDA key by simulating Solana's
/// findProgramAddress locally.
fn derive_domain_pda(domain_part: &str, parent: &str) -> Result<String, String> {
    let name_hash = hash_name(domain_part);
    let _parent_bytes = decode_base58(parent)?;

    for bump in (0..=255u8).rev() {
        let mut input = Vec::new();
        let hash_bytes = hex_to_bytes(&name_hash);
        input.extend_from_slice(&hash_bytes);
        input.extend_from_slice(&[0u8; 32]);
        input.push(bump);

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
fn derive_name_account_key(name_part: &str, parent_key: &str) -> Result<String, String> {
    let hashed_name = hash_name_with_prefix(name_part);
    let parent_bytes = decode_base58(parent_key)?;

    for bump in (0..=255u8).rev() {
        let mut input = Vec::new();
        let hash_bytes = hex_to_bytes(&hashed_name);
        input.extend_from_slice(&hash_bytes);
        input.extend_from_slice(&[0u8; 32]);
        input.extend_from_slice(&parent_bytes);
        input.push(bump);

        let program_bytes = decode_base58(SNS_PROGRAM_ID)?;
        input.extend_from_slice(&program_bytes);

        let hash = simple_sha256(&input);
        if !is_on_curve(&hash) {
            return Ok(bs58_encode(&hash));
        }
    }

    Err("could not find valid PDA for name account".to_string())
}

fn is_on_curve(bytes: &[u8; 32]) -> bool {
    let q: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58,
        0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
    ];

    let bytes_le = {
        let mut b = [0u8; 32];
        for (i, &byte) in bytes.iter().enumerate() {
            b[31 - i] = byte;
        }
        b
    };

    for i in (0..32).rev() {
        if bytes_le[i] > q[i] { return true; }
        if bytes_le[i] < q[i] { break; }
    }
    false
}

/// Fetch account data using RpcClient::get_ui_account_with_config with base64 encoding.
fn fetch_account_data<T: RpcTransport>(
    pubkey: &str,
    client: &RpcClient<T>,
) -> Result<Vec<u8>, String> {
    let pk = Pubkey::from_str(pubkey).map_err(|e| format!("invalid pubkey: {e}"))?;

    let config = RpcAccountInfoConfig {
        encoding: Some(solana_client_wasip2::rpc::config::UiAccountEncoding::Base64),
        commitment: Some(solana_client_wasip2::CommitmentConfig::confirmed()),
        ..Default::default()
    };

    let account = client
        .get_ui_account_with_config(&pk, config)
        .map_err(|e| format!("RPC error: {e}"))?;

    let ui_account = account
        .value
        .ok_or_else(|| format!("name account '{pubkey}' not found (domain may not exist)"))?;

    let (data_str, _encoding) = match ui_account.data {
        solana_client_wasip2::rpc::response::UiAccountData::Binary(s, enc) => (s, enc),
        _ => return Err("unexpected account data format".to_string()),
    };

    base64_decode(&data_str)
}

fn base64_decode(data: &str) -> Result<Vec<u8>, String> {
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
        if valid >= 2 { bytes.push(((buf >> 16) & 0xFF) as u8); }
        if valid >= 3 { bytes.push(((buf >> 8) & 0xFF) as u8); }
        if valid >= 4 { bytes.push((buf & 0xFF) as u8); }
    }

    Ok(bytes)
}

fn hash_name(name: &str) -> String {
    let hash = simple_sha256(name.as_bytes());
    bytes_to_hex(&hash)
}

fn hash_name_with_prefix(name: &str) -> String {
    let input = format!("{}{}", HASH_PREFIX, name);
    let hash = simple_sha256(input.as_bytes());
    bytes_to_hex(&hash)
}

fn decode_base58(addr: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    let mut result = vec![0u8; 0];

    for c in addr.chars() {
        let val = ALPHABET
            .iter()
            .position(|&a| a == c as u8)
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
    if hex.len() % 2 != 0 { return Vec::new(); }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0))
        .collect()
}

fn simple_sha256(data: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09e667u32, 0xbb67ae85u32, 0x3c6ef372u32, 0xa54ff53au32,
        0x510e527fu32, 0x9b05688cu32, 0x1f83d9abu32, 0x5be0cd19u32,
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

    let pad_len = ((55 - msg_len % 64 + 64) % 64) + 9;
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

        let mut a: u32 = state[0];
        let mut b: u32 = state[1];
        let mut c: u32 = state[2];
        let mut d: u32 = state[3];
        let mut e: u32 = state[4];
        let mut f: u32 = state[5];
        let mut g: u32 = state[6];
        let mut h: u32 = state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g; g = f; f = e; e = d.wrapping_add(temp1);
            d = c; c = b; b = a; a = temp1.wrapping_add(temp2);
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

fn bs58_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    let mut zeros = 0;
    for &b in data {
        if b == 0 { zeros += 1; } else { break; }
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

    let mut result = String::new();
    result.push_str(&"1".repeat(zeros));
    for digit in b58.iter().rev() {
        if *digit != 0 || result.len() > zeros {
            result.push(ALPHABET[*digit as usize] as char);
        }
    }

    if result.is_empty() { result.push('1'); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_client_wasip2::MockTransport;

    fn make_mock_transport_with_owner(owner_pk: &str) -> MockTransport {
        let pk = Pubkey::from_str(owner_pk).unwrap();
        let owner_bytes = pk.to_bytes();
        let mut account_data = vec![0u8; 96];
        account_data[32..64].copy_from_slice(&owner_bytes);
        let b64 = base64_encode(&account_data);

        let json = format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"context":{{"slot":1}},"value":{{"data":["{}","base64"],"executable":false,"lamports":1000000,"owner":"namesLPneVptA9Z5rqUDD9tMTWEJwofgaYwp8cawRkX","rentEpoch":0}}}}}}"#,
            b64
        );
        MockTransport::success(&json)
    }

    fn base64_encode(data: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
            let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
            let triple = (b0 << 16) | (b1 << 8) | b2;
            result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
            result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                result.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
            } else {
                result.push('=');
            }
            if chunk.len() > 2 {
                result.push(ALPHABET[(triple & 0x3F) as usize] as char);
            } else {
                result.push('=');
            }
        }
        result
    }

    fn bs58_encode_for_b64(data: &[u8]) -> String {
        bs58_encode(data)
    }

    #[test]
    fn test_resolve_domain() {
        let mock = make_mock_transport_with_owner("FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp");
        let client = RpcClient::new_with_transport(DEFAULT_RPC_URL, mock);
        let result = resolve_domain("lucas.sol", &client);
        assert!(result.is_ok(), "failed: {:?}", result.err());
        assert_eq!(result.unwrap(), "FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp");
    }

    #[test]
    fn test_resolve_domain_trimmed() {
        let mock = make_mock_transport_with_owner("FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp");
        let client = RpcClient::new_with_transport(DEFAULT_RPC_URL, mock);
        let result = resolve_domain("  lucas.sol  ", &client);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_domain_without_dot_sol() {
        let mock = make_mock_transport_with_owner("FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp");
        let client = RpcClient::new_with_transport(DEFAULT_RPC_URL, mock);
        let result = resolve_domain("lucas", &client);
        assert!(result.is_ok());
    }

    #[test]
    fn test_invalid_domain_too_many_parts() {
        let mock = MockTransport::success(r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":1},"value":null}}"#);
        let client = RpcClient::new_with_transport(DEFAULT_RPC_URL, mock);
        let result = resolve_domain("a.b.c.sol", &client);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too many parts"));
    }

    #[test]
    fn test_empty_domain() {
        let mock = MockTransport::success("{}");
        let client = RpcClient::new_with_transport(DEFAULT_RPC_URL, mock);
        let result = resolve_domain("", &client);
        assert!(result.is_err());
    }

    #[test]
    fn test_bs58_roundtrip() {
        let addr = "FzW7s6xGLSxDkHnAqqxLjQTjPnB7YKLNjNV3LhUBMhPp";
        let bytes = decode_base58(addr).unwrap();
        let reencoded = bs58_encode(&bytes);
        assert_eq!(addr, reencoded);
    }

    #[test]
    fn test_sha256_known() {
        let hash = simple_sha256(b"abc");
        let hex = bytes_to_hex(&hash);
        assert_eq!(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn test_hash_name_with_prefix() {
        let h = hash_name_with_prefix("lucas");
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn test_config_from_section() {
        let mut section = HashMap::new();
        section.insert("rpc_url".to_string(), "https://custom.rpc.com".to_string());
        let cfg = SnsConfig::from_section(&section);
        assert_eq!(cfg.rpc_url, "https://custom.rpc.com");
    }

    #[test]
    fn test_config_default_url() {
        let cfg = SnsConfig::from_section(&HashMap::new());
        assert_eq!(cfg.rpc_url, DEFAULT_RPC_URL);
    }
}
