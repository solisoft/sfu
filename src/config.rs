//! Configuration: a small TOML file + env overrides, in the spirit of
//! soli-proxy's config. Everything has a dev-friendly default so
//! `cargo run` works out of the box (loopback, unauthenticated).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// IP advertised in the ICE host candidate. MUST be the address clients
    /// can reach over UDP (the public IP in production, 127.0.0.1 in dev).
    pub public_ip: IpAddr,

    /// Single UDP port all media flows through (every peer is multiplexed on
    /// it; ICE-lite demuxes by STUN ufrag / SRTP by source address).
    pub udp_port: u16,

    /// HTTP control-plane bind address (offer/answer exchange, group updates).
    /// Front it with soli-proxy for TLS; media UDP goes direct, NOT proxied.
    pub control_addr: SocketAddr,

    /// HMAC secret for session tokens. Mirrors bonfire's `GatherWs` scheme,
    /// so set it to the same value as the app's SOLI_WEBHOOK_SECRET (or mint
    /// SFU tokens with a dedicated secret on both sides).
    /// Env override: SOLI_SFU_SECRET, falling back to SOLI_WEBHOOK_SECRET.
    pub secret: String,

    /// Dev mode: accept `dev.<user_id>.<room>` pseudo-tokens with no
    /// signature. NEVER enable in production.
    pub allow_unauthenticated: bool,

    /// Downstream slots pre-allocated per subscriber (see engine::Slot).
    /// These bound how many simultaneous publishers one client can receive.
    pub audio_slots: usize,
    pub video_slots: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            public_ip: "127.0.0.1".parse().unwrap(),
            udp_port: 3478,
            control_addr: "127.0.0.1:9300".parse().unwrap(),
            secret: String::new(),
            allow_unauthenticated: false,
            audio_slots: 8,
            video_slots: 4,
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut cfg = match path {
            Some(p) => {
                let raw = std::fs::read_to_string(p)
                    .with_context(|| format!("reading config {}", p.display()))?;
                toml::from_str(&raw).with_context(|| format!("parsing {}", p.display()))?
            }
            None => Config::default(),
        };
        if let Ok(secret) = std::env::var("SOLI_SFU_SECRET") {
            cfg.secret = secret;
        } else if cfg.secret.is_empty() {
            if let Ok(secret) = std::env::var("SOLI_WEBHOOK_SECRET") {
                cfg.secret = secret;
            }
        }
        if let Ok(ip) = std::env::var("SOLI_SFU_PUBLIC_IP") {
            cfg.public_ip = ip.parse().context("SOLI_SFU_PUBLIC_IP")?;
        }
        if cfg.secret.is_empty() && !cfg.allow_unauthenticated {
            anyhow::bail!(
                "no token secret configured: set SOLI_SFU_SECRET (or SOLI_WEBHOOK_SECRET), \
                 or enable allow_unauthenticated for local dev"
            );
        }
        Ok(cfg)
    }
}
