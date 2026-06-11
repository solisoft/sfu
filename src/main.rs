//! soli-sfu: a lightweight group SFU for the Soli ecosystem.
//!
//! Topology:
//!   browser --HTTPS (via soli-proxy)--> control API (axum, this process)
//!   browser --UDP, direct, ONE port--> media engine (str0m, ICE-lite)
//!
//! Run as its OWN service, not inside soli-proxy: media sessions are
//! long-lived UDP state and must survive proxy blue-green deployments.

mod auth;
mod config;
mod control;
mod engine;

use anyhow::{Context, Result};
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use tokio::sync::mpsc;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "soli_sfu=info".into()),
        )
        .init();

    // `soli-sfu mint-token <user_id> <room> [ttl_secs]` - mint a token from
    // the configured secret (handy for curl / the test page).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("mint-token") {
        let user = args
            .get(2)
            .context("usage: mint-token <user_id> <room> [ttl]")?;
        let room = args
            .get(3)
            .context("usage: mint-token <user_id> <room> [ttl]")?;
        let ttl: u64 = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(3600);
        let secret = std::env::var("SOLI_SFU_SECRET")
            .or_else(|_| std::env::var("SOLI_WEBHOOK_SECRET"))
            .context("set SOLI_SFU_SECRET or SOLI_WEBHOOK_SECRET")?;
        println!("{}", auth::mint(user, room, &secret, ttl));
        return Ok(());
    }

    let config_path = args.get(1).map(PathBuf::from);
    let cfg = config::Config::load(config_path.as_deref())?;
    if cfg.allow_unauthenticated {
        tracing::warn!("allow_unauthenticated is ON - dev tokens accepted, do not expose this");
    }

    // Media socket: bind on all interfaces, advertise the configured public
    // IP in the ICE candidate.
    let bind: SocketAddr = format!("0.0.0.0:{}", cfg.udp_port).parse()?;
    let socket = UdpSocket::bind(bind).with_context(|| format!("binding UDP {bind}"))?;
    let advertised = SocketAddr::new(cfg.public_ip, cfg.udp_port);
    tracing::info!("media: udp {bind} (advertised as {advertised}, ice-lite)");

    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let media = engine::Engine::new(socket, advertised, cmd_rx, cfg.audio_slots, cfg.video_slots)?;
    std::thread::Builder::new()
        .name("sfu-media".into())
        .spawn(move || media.run())
        .context("spawning media thread")?;

    // Control plane on the tokio runtime.
    let control_addr = cfg.control_addr;
    let state = control::AppState {
        cfg: std::sync::Arc::new(cfg),
        cmd_tx,
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(control_addr)
            .await
            .with_context(|| format!("binding control {control_addr}"))?;
        tracing::info!("control: http://{control_addr}");
        axum::serve(listener, control::router(state))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("shutting down");
            })
            .await
            .context("control server")
    })
}
