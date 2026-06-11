//! Session-token verification, byte-compatible with bonfire's `GatherWs`
//! scheme so the Soli side can mint tokens with the existing builtins:
//!
//! ```soli
//! exp     = DateTime.utc().to_unix() + 3600
//! payload = Base64.encode(user_id) + ":" + Base64.encode(room) + ":" + str(exp)
//! signed  = "sfu1." + payload
//! token   = signed + "." + Crypto.hmac(signed, secret)   # HMAC-SHA256, lowercase hex
//! ```
//!
//! Dev mode additionally accepts `dev.<user_id>.<room>` with no signature.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub user_id: String,
    pub room: String,
}

fn hmac_hex(message: &str, key: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("hmac accepts any key len");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn b64_decode(s: &str) -> Option<String> {
    // Soli's Base64.encode pads; be liberal and accept unpadded too.
    let engine = base64::engine::general_purpose::STANDARD;
    let no_pad = base64::engine::general_purpose::STANDARD_NO_PAD;
    let bytes = engine
        .decode(s)
        .or_else(|_| no_pad.decode(s.trim_end_matches('=')))
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Verify a token. `allow_unauthenticated` additionally admits dev
/// pseudo-tokens (`dev.<user>.<room>`) for local testing without a secret.
pub fn verify(token: &str, secret: &str, allow_unauthenticated: bool) -> Option<Claims> {
    if allow_unauthenticated {
        if let Some(rest) = token.strip_prefix("dev.") {
            let mut it = rest.splitn(2, '.');
            let user_id = it.next()?.to_string();
            let room = it.next()?.to_string();
            if !user_id.is_empty() && !room.is_empty() {
                return Some(Claims { user_id, room });
            }
            return None;
        }
    }
    if secret.is_empty() {
        return None;
    }
    let mut parts = token.splitn(3, '.');
    let (prefix, payload, sig) = (parts.next()?, parts.next()?, parts.next()?);
    if prefix != "sfu1" {
        return None;
    }
    let signed = format!("{prefix}.{payload}");
    if !constant_time_eq(&hmac_hex(&signed, secret), sig) {
        return None;
    }
    let fields: Vec<&str> = payload.split(':').collect();
    if fields.len() != 3 {
        return None;
    }
    let exp: u64 = fields[2].parse().ok()?;
    if exp <= now_unix() {
        return None;
    }
    let user_id = b64_decode(fields[0])?;
    let room = b64_decode(fields[1])?;
    if user_id.is_empty() || room.is_empty() {
        return None;
    }
    Some(Claims { user_id, room })
}

/// Mint a token (used by tests and the CLI; production tokens come from the
/// Soli app, which holds the same secret).
pub fn mint(user_id: &str, room: &str, secret: &str, ttl_secs: u64) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    let payload = format!(
        "{}:{}:{}",
        engine.encode(user_id),
        engine.encode(room),
        now_unix() + ttl_secs
    );
    let signed = format!("sfu1.{payload}");
    let sig = hmac_hex(&signed, secret);
    format!("{signed}.{sig}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let token = mint("user-42", "spatial:acme", "s3cret", 60);
        let claims = verify(&token, "s3cret", false).expect("valid");
        assert_eq!(claims.user_id, "user-42");
        assert_eq!(claims.room, "spatial:acme");
    }

    #[test]
    fn rejects_bad_signature() {
        let token = mint("user-42", "spatial:acme", "s3cret", 60);
        assert!(verify(&token, "other-secret", false).is_none());
        let tampered = format!("{}{}", &token[..token.len() - 2], "zz");
        assert!(verify(&tampered, "s3cret", false).is_none());
    }

    #[test]
    fn rejects_expired() {
        let engine = base64::engine::general_purpose::STANDARD;
        let payload = format!(
            "{}:{}:{}",
            engine.encode("u"),
            engine.encode("r"),
            now_unix() - 1
        );
        let signed = format!("sfu1.{payload}");
        let sig = hmac_hex(&signed, "s3cret");
        assert!(verify(&format!("{signed}.{sig}"), "s3cret", false).is_none());
    }

    #[test]
    fn matches_soli_crypto_hmac() {
        // Pin the exact construction Soli's Crypto.hmac produces
        // (HMAC-SHA256, lowercase hex) so a drift on either side fails here.
        assert_eq!(
            hmac_hex("sfu1.payload", "key"),
            "2f89b853b35082aea9fa909b8b558c804b6250c1dd94bf3c66cb8877ee8ca1d8"
        );
    }

    #[test]
    fn dev_tokens_only_when_allowed() {
        assert!(verify("dev.u1.spatial:acme", "", true).is_some());
        assert!(verify("dev.u1.spatial:acme", "s3cret", false).is_none());
    }
}
