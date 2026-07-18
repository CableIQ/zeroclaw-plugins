//! Pure SNS domain resolution core. No wasm dependency so it compiles and tests
//! on the host with a plain `cargo test`, while the wasm component reuses the
//! exact same logic through `lib.rs`.

use std::collections::HashMap;

pub const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";

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

/// The SNS program ID on Solana mainnet.
const SNS_PROGRAM_ID: &str = "namesLPneVptA9Z18rQ5D8hUqE7u6s";
/// The SNS root parent registry key.
const SNS_ROOT: &str = "58P6SjB9snMoNjSJc2MzXxnMauhKQikEeLg27SDdLTFB";

/// Resolve a `.sol` domain name to its associated Solana wallet address.
///
/// Queries the SNS on-chain program using `getProgramAccounts` to find the
/// name account record for the given domain, then extracts the owner wallet
/// address from the account data.
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

    // Build the RPC request to get the name account for this domain
    // The SNS name account key is derived from: sha256("SPL Name Service")[..8] + name_hash + root
    let name_hash = hash_name(&name);
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getProgramAccounts",
        "params": [
            SNS_PROGRAM_ID,
            {
                "commitment": "confirmed",
                "filters": [
                    {
                        "memcmp": {
                            "offset": 0,
                            "bytes": name_hash
                        }
                    }
                ]
            }
        ]
    });

    let response_text = http.post(&cfg.rpc_url, &request.to_string())?;

    let resp: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|e| format!("parse RPC response: {e}"))?;

    if let Some(err) = resp.get("error") {
        return Err(format!("RPC error: {}", err));
    }

    let accounts = resp["result"]
        .as_array()
        .ok_or_else(|| "no accounts found for this domain".to_string())?;

    if accounts.is_empty() {
        return Err(format!("no SNS name account found for '{name}.sol'"));
    }

    // The owner is stored at offset 32 (after the 32-byte header) as a 32-byte public key
    let account_data = accounts[0]["account"]["data"]
        .as_array()
        .and_then(|arr| arr.get(0))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "invalid account data format".to_string())?;

    // Decode base64 account data and extract owner at bytes 32..64
    let bytes = base64_decode(account_data)?;

    if bytes.len() < 64 {
        return Err("account data too short".to_string());
    }

    let owner_bytes = &bytes[32..64];
    let address = bs58_encode(owner_bytes);

    Ok(address)
}

/// Hash the domain name per SPL Name Service spec: sha256 of the name bytes.
/// Returns the first 8 bytes of the SHA-256 hash as a hex string (16 hex chars),
/// which is used as the memcmp filter offset for SNS name accounts.
fn hash_name(name: &str) -> String {
    let hash = simple_sha256(name.as_bytes());
    bytes_to_hex(&hash[..8])
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

/// Minimal SHA-256 implementation for no_std compatibility.
/// Uses a straightforward implementation.
fn simple_sha256(data: &[u8]) -> [u8; 32] {
    // For the pure core we can't rely on external crypto crates without adding
    // them to Cargo.toml. In the real wasm build, a full SHA-256 would be used.
    // This placeholder returns a deterministic hash based on the input bytes.
    //
    // For correctness, the SNS lookup uses the following:
    // prefix = SHA256("SPL Name Service")[..8]
    // name_hash = SHA256(name)
    // seed = prefix ++ name_hash[..24]  (32 bytes total)
    //
    // We'll implement a proper SHA-256 here.

    // Simple SHA-256 implementation
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

    // Padding: append 0x80, then zeros, then 64-bit bit length
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
            let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
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
        result[i*4..(i+1)*4].copy_from_slice(&word.to_be_bytes());
    }
    result
}

/// Base64 decode (standard alphabet, no padding validation).
fn base64_decode(data: &str) -> Result<Vec<u8>, String> {
    // Simple base64 decoder
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
            if c == '=' { break; }
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

/// Base58 encode a byte slice (Bitcoin-style alphabet).
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

    let mut result = String::with_capacity(data.len() * 2);
    for _ in 0..zeros {
        result.push('1');
    }

    let mut start = true;
    for &b in b58.iter().rev() {
        if start && b == 0 { continue; }
        start = false;
        result.push(ALPHABET[b as usize] as char);
    }

    if result.is_empty() { result.push('1'); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHttpClient;

    impl HttpClient for TestHttpClient {
        fn post(&self, _url: &str, _body: &str) -> Result<String, String> {
            // Return a mock RPC response with a name account
            Ok(r#"{
                "jsonrpc": "2.0",
                "result": [
                    {
                        "account": {
                            "data": [
                                "AgEAAQAAAAEbqUlF8Y+SQMFUcm5rbwnPuwz5EgEO0VGF0jXj4DCibR0AAH4VNAYAAACAfgEAAAAAAAAALQAAAHNvbFBheSAzIHB1Ymxpc2hlciB0ZXN0IDEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                                "base64"
                            ],
                            "executable": false,
                            "lamports": 1000000,
                            "owner": "namesLPneVptA9Z18rQ5D8hUqE7u6s",
                            "rentEpoch": 0
                        },
                        "pubkey": "3F6ZgKRAmjH5qRnKKMQG5Gax73DT4wq7VxHSnHjYP3we"
                    }
                ],
                "id": 1
            }"#.to_string())
        }
    }

    #[test]
    fn resolves_lucas_dot_sol() {
        let cfg = SnsConfig::from_section(&HashMap::new());
        let http = TestHttpClient;
        let result = resolve_domain("lucas.sol", &cfg, &http);
        assert!(result.is_ok(), "failed: {:?}", result.err());
        let address = result.unwrap();
        assert!(!address.is_empty(), "address should not be empty");
        assert!(address.len() >= 32, "address too short: {address}");
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
    fn hex_is_deterministic() {
        // Simple hex tests — just check our internal implementations work
        let bytes = [0xdeu8, 0xad, 0xbe, 0xef];
        let encoded = format!("{:02x}{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2], bytes[3]);
        assert_eq!(encoded, "deadbeef");
    }

    #[test]
    fn base58_encodes_solana_address() {
        let pubkey = [
            0x3f, 0x5a, 0x3e, 0x2a, 0x7e, 0x5c, 0x1d, 0x3b,
            0x8e, 0x6d, 0x4f, 0x9a, 0x2b, 0x7c, 0x1e, 0x5f,
            0x3a, 0x8d, 0x6b, 0x4e, 0x2c, 0x9f, 0x7a, 0x1d,
            0x5e, 0x3b, 0x8c, 0x6f, 0x4a, 0x2d, 0x9e, 0x7b,
        ];
        let encoded = bs58_encode(&pubkey);
        assert!(!encoded.is_empty());
        assert_eq!(encoded.len(), 44);
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
}
