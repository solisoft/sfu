//! The media engine: ONE OS thread, ONE UDP socket, all peers multiplexed.
//!
//! str0m is sans-IO: each peer is an `Rtc` state machine we feed with UDP
//! input and drain via `poll_output()`. The loop is the classic shape from
//! str0m's chat example, extended with:
//!
//!   * rooms + per-subscriber "peers" sets (bonfire's proximity groups), so
//!     a publisher's media only fans out to the people in their conversation
//!     - never the whole office;
//!   * slot-based downstream: every subscriber pre-allocates N recvonly
//!     m-lines in its offer (audio_slots/video_slots). Publishers are mapped
//!     onto free slots - NO renegotiation, ever. With proximity groups of
//!     2-8 people, slots never run out in practice; if they do we steal the
//!     longest-idle slot.
//!
//! The control plane (axum, in main.rs) talks to this thread through an mpsc
//! channel; replies ride oneshot channels. The UDP read timeout doubles as
//! the command-poll tick, so a new session is admitted within ~20ms.

use crate::auth::Claims;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::media::{Direction, KeyframeRequestKind, MediaData, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::sync::{mpsc, oneshot};

/// Max UDP datagram we accept (Ethernet MTU-ish; WebRTC keeps under this).
const MAX_DATAGRAM: usize = 2000;
/// Socket read timeout = how often we re-check commands & timeouts.
const TICK: Duration = Duration::from_millis(20);
/// A slot whose publisher said nothing for this long may be stolen.
const SLOT_IDLE_STEAL: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Control-plane commands
// ---------------------------------------------------------------------------

pub enum Cmd {
    /// Accept an SDP offer for an authenticated user; reply with the answer.
    NewSession {
        claims: Claims,
        offer_sdp: String,
        /// user_ids this subscriber should hear. None = everyone in the room.
        peers: Option<Vec<String>>,
        reply: oneshot::Sender<Result<NewSession>>,
    },
    /// Replace the peer set of a session (conversation group changed shape).
    SetPeers {
        session_id: u64,
        user_id: String,
        peers: Option<Vec<String>>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Tear a session down (client left the office).
    Drop {
        session_id: u64,
        user_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Lightweight introspection for /v1/stats.
    Stats {
        reply: oneshot::Sender<StatsSnapshot>,
    },
    /// The session's current slot bindings: slot mid -> publisher user_id.
    /// Clients poll this to label tiles (the slot model carries no identity).
    Slots {
        session_id: u64,
        user_id: String,
        reply: oneshot::Sender<Result<HashMap<String, String>>>,
    },
}

pub struct NewSession {
    pub session_id: u64,
    pub answer_sdp: String,
}

#[derive(serde::Serialize)]
pub struct StatsSnapshot {
    pub sessions: usize,
    pub rooms: HashMap<String, usize>,
}

// ---------------------------------------------------------------------------
// Per-peer state
// ---------------------------------------------------------------------------

/// One pre-allocated downstream m-line of a subscriber, dynamically bound to
/// a publisher's upstream track.
struct Slot {
    mid: Mid,
    kind: MediaKind,
    /// (publisher session id, publisher's upstream mid) currently mapped here.
    src: Option<(u64, Mid)>,
    last_data: Instant,
}

struct Client {
    id: u64,
    user_id: String,
    room: String,
    /// Who this client hears. None = everyone in the room.
    peers: Option<Vec<String>>,
    rtc: Rtc,
    /// Their upstream (publishing) m-lines.
    uplinks: HashMap<Mid, MediaKind>,
    /// Our pre-allocated downstream m-lines toward them.
    slots: Vec<Slot>,
    connected: bool,
    _data_channels: Vec<ChannelId>,
}

impl Client {
    fn hears(&self, publisher_user: &str) -> bool {
        match &self.peers {
            None => true,
            Some(list) => list.iter().any(|u| u == publisher_user),
        }
    }

    /// Find the slot currently bound to (publisher, mid), or bind a free /
    /// stealable one. Returns the slot mid to write on.
    fn slot_for(&mut self, src: (u64, Mid), kind: MediaKind, now: Instant) -> Option<Mid> {
        if let Some(s) = self
            .slots
            .iter_mut()
            .find(|s| s.kind == kind && s.src == Some(src))
        {
            s.last_data = now;
            return Some(s.mid);
        }
        // a free slot first, otherwise steal the longest-idle one
        let candidate = self
            .slots
            .iter_mut()
            .filter(|s| s.kind == kind)
            .filter(|s| s.src.is_none() || now.duration_since(s.last_data) > SLOT_IDLE_STEAL)
            .min_by_key(|s| (s.src.is_some(), s.last_data));
        let s = candidate?;
        s.src = Some(src);
        s.last_data = now;
        Some(s.mid)
    }

    /// Unbind every slot fed by a departed publisher.
    fn release_publisher(&mut self, publisher_id: u64) {
        for s in self.slots.iter_mut() {
            if matches!(s.src, Some((id, _)) if id == publisher_id) {
                s.src = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct Engine {
    socket: UdpSocket,
    advertised: std::net::SocketAddr,
    clients: Vec<Client>,
    next_id: u64,
    cmd_rx: mpsc::Receiver<Cmd>,
    audio_slots: usize,
    video_slots: usize,
}

impl Engine {
    pub fn new(
        socket: UdpSocket,
        advertised: std::net::SocketAddr,
        cmd_rx: mpsc::Receiver<Cmd>,
        audio_slots: usize,
        video_slots: usize,
    ) -> Result<Self> {
        socket
            .set_read_timeout(Some(TICK))
            .context("set_read_timeout")?;
        Ok(Self {
            socket,
            advertised,
            clients: Vec::new(),
            next_id: 1,
            cmd_rx,
            audio_slots,
            video_slots,
        })
    }

    /// Blocking loop - run on a dedicated thread.
    pub fn run(mut self) {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            self.drain_commands();
            self.drive_clients();
            self.read_socket(&mut buf);
            self.reap_dead();
        }
    }

    // ---- control plane ----------------------------------------------------

    fn drain_commands(&mut self) {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                Cmd::NewSession {
                    claims,
                    offer_sdp,
                    peers,
                    reply,
                } => {
                    let res = self.accept_session(claims, &offer_sdp, peers);
                    let _ = reply.send(res);
                }
                Cmd::SetPeers {
                    session_id,
                    user_id,
                    peers,
                    reply,
                } => {
                    let res = match self
                        .clients
                        .iter_mut()
                        .find(|c| c.id == session_id && c.user_id == user_id)
                    {
                        Some(c) => {
                            c.peers = peers;
                            Ok(())
                        }
                        None => Err(anyhow::anyhow!("unknown session")),
                    };
                    let _ = reply.send(res);
                }
                Cmd::Drop {
                    session_id,
                    user_id,
                    reply,
                } => {
                    let before = self.clients.len();
                    self.clients
                        .retain(|c| !(c.id == session_id && c.user_id == user_id));
                    if self.clients.len() < before {
                        self.publisher_gone(session_id);
                        let _ = reply.send(Ok(()));
                    } else {
                        let _ = reply.send(Err(anyhow::anyhow!("unknown session")));
                    }
                }
                Cmd::Slots {
                    session_id,
                    user_id,
                    reply,
                } => {
                    let res = match self
                        .clients
                        .iter()
                        .position(|c| c.id == session_id && c.user_id == user_id)
                    {
                        Some(idx) => {
                            // slot mid -> the user_id of the session feeding it
                            let mut map = HashMap::new();
                            for s in &self.clients[idx].slots {
                                let Some((pub_id, _)) = s.src else { continue };
                                if let Some(p) = self.clients.iter().find(|c| c.id == pub_id) {
                                    map.insert(s.mid.to_string(), p.user_id.clone());
                                }
                            }
                            Ok(map)
                        }
                        None => Err(anyhow::anyhow!("unknown session")),
                    };
                    let _ = reply.send(res);
                }
                Cmd::Stats { reply } => {
                    let mut rooms: HashMap<String, usize> = HashMap::new();
                    for c in &self.clients {
                        *rooms.entry(c.room.clone()).or_default() += 1;
                    }
                    let _ = reply.send(StatsSnapshot {
                        sessions: self.clients.len(),
                        rooms,
                    });
                }
            }
        }
    }

    fn accept_session(
        &mut self,
        claims: Claims,
        offer_sdp: &str,
        peers: Option<Vec<String>>,
    ) -> Result<NewSession> {
        // One session per (user, room): a reconnect replaces the old leg.
        if let Some(old) = self
            .clients
            .iter()
            .position(|c| c.user_id == claims.user_id && c.room == claims.room)
        {
            let gone = self.clients.remove(old).id;
            self.publisher_gone(gone);
        }

        let mut rtc = Rtc::builder()
            // ICE-lite: we are the reachable side; we only answer STUN checks.
            .set_ice_lite(true)
            .build(Instant::now());
        let candidate = Candidate::host(self.advertised, Protocol::Udp)
            .map_err(|e| anyhow::anyhow!("host candidate: {e}"))?;
        rtc.add_local_candidate(candidate);

        let offer =
            SdpOffer::from_sdp_string(offer_sdp).map_err(|e| anyhow::anyhow!("bad offer: {e}"))?;
        let answer: SdpAnswer = rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| anyhow::anyhow!("accept_offer: {e}"))?;

        let id = self.next_id;
        self.next_id += 1;
        tracing::info!(session = id, user = %claims.user_id, room = %claims.room, "session created");
        self.clients.push(Client {
            id,
            user_id: claims.user_id,
            room: claims.room,
            peers,
            rtc,
            uplinks: HashMap::new(),
            slots: Vec::new(),
            connected: false,
            _data_channels: Vec::new(),
        });

        Ok(NewSession {
            session_id: id,
            answer_sdp: answer.to_sdp_string(),
        })
    }

    // ---- media plane ------------------------------------------------------

    /// Poll every client until all return Timeout, forwarding media between
    /// them. Two passes per event to satisfy the borrow checker: collect from
    /// one client, then distribute to the others.
    fn drive_clients(&mut self) {
        let now = Instant::now();
        // Forwarding work discovered while polling: (publisher idx, data).
        loop {
            let mut progressed = false;
            for i in 0..self.clients.len() {
                match self.clients[i].rtc.poll_output() {
                    Ok(Output::Timeout(_)) => continue,
                    Ok(Output::Transmit(t)) => {
                        if let Err(e) = self.socket.send_to(&t.contents, t.destination) {
                            tracing::debug!("udp send_to {}: {}", t.destination, e);
                        }
                        progressed = true;
                    }
                    Ok(Output::Event(event)) => {
                        self.handle_event(i, event, now);
                        progressed = true;
                    }
                    Err(e) => {
                        tracing::warn!(session = self.clients[i].id, "rtc error: {e}");
                        self.clients[i].rtc.disconnect();
                        progressed = true;
                    }
                }
            }
            // feed wall-clock time so timers (DTLS, RTCP, stats) advance
            for c in self.clients.iter_mut() {
                let _ = c.rtc.handle_input(Input::Timeout(Instant::now()));
            }
            if !progressed {
                break;
            }
        }
    }

    fn handle_event(&mut self, idx: usize, event: Event, now: Instant) {
        match event {
            Event::Connected => {
                self.clients[idx].connected = true;
                tracing::info!(session = self.clients[idx].id, "connected");
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                tracing::info!(session = self.clients[idx].id, "ice disconnected");
                self.clients[idx].rtc.disconnect();
            }
            Event::IceConnectionStateChange(_) => {}
            Event::MediaAdded(added) => {
                let c = &mut self.clients[idx];
                // Directions are from the CLIENT's point of view in the offer;
                // str0m reports our local view: RecvOnly = they publish to us,
                // SendOnly = a downstream slot we may write to.
                match added.direction {
                    Direction::RecvOnly | Direction::SendRecv => {
                        c.uplinks.insert(added.mid, added.kind);
                        tracing::debug!(session = c.id, mid = %added.mid, kind = ?added.kind, "uplink");
                    }
                    Direction::SendOnly => {
                        // cap what one client may pre-allocate (a hostile
                        // offer with hundreds of m-lines must not balloon us)
                        let cap = match added.kind {
                            MediaKind::Audio => self.audio_slots,
                            MediaKind::Video => self.video_slots,
                        };
                        let have = c.slots.iter().filter(|s| s.kind == added.kind).count();
                        if have >= cap {
                            tracing::debug!(session = c.id, mid = %added.mid, "slot over cap, ignored");
                        } else {
                            c.slots.push(Slot {
                                mid: added.mid,
                                kind: added.kind,
                                src: None,
                                last_data: now,
                            });
                            tracing::debug!(session = c.id, mid = %added.mid, kind = ?added.kind, "slot");
                        }
                    }
                    Direction::Inactive => {}
                }
            }
            Event::MediaData(data) => self.forward(idx, data, now),
            Event::KeyframeRequest(req) => {
                // A subscriber asks for a keyframe on one of its slots:
                // relay to the publisher currently feeding that slot.
                let src = self.clients[idx]
                    .slots
                    .iter()
                    .find(|s| s.mid == req.mid)
                    .and_then(|s| s.src);
                if let Some((pub_id, pub_mid)) = src {
                    self.request_keyframe(pub_id, pub_mid, req.kind);
                }
            }
            _ => {}
        }
    }

    /// Fan a publisher's media out to every connected room-mate whose peer
    /// set includes the publisher.
    fn forward(&mut self, from_idx: usize, data: MediaData, now: Instant) {
        let (from_id, from_user, from_room) = {
            let c = &self.clients[from_idx];
            (c.id, c.user_id.clone(), c.room.clone())
        };
        let kind = match self.clients[from_idx].uplinks.get(&data.mid) {
            Some(k) => *k,
            None => return, // data on a non-uplink mid: ignore
        };

        let mut want_keyframe = false;
        for i in 0..self.clients.len() {
            if i == from_idx {
                continue;
            }
            {
                let sub = &self.clients[i];
                if !sub.connected || sub.room != from_room || !sub.hears(&from_user) {
                    continue;
                }
            }
            let Some(slot_mid) = self.clients[i].slot_for((from_id, data.mid), kind, now) else {
                continue; // no slot available - drop for this subscriber
            };
            let newly_bound = {
                // freshly bound video slots need a keyframe to start decoding
                let sub = &self.clients[i];
                kind == MediaKind::Video
                    && sub.slots.iter().any(|s| {
                        s.mid == slot_mid
                            && s.last_data == now
                            && s.src == Some((from_id, data.mid))
                    })
            };
            let sub = &mut self.clients[i];
            let Some(writer) = sub.rtc.writer(slot_mid) else {
                continue;
            };
            let Some(pt) = writer.match_params(data.params) else {
                continue; // no compatible codec on this slot
            };
            if let Err(e) = writer.write(pt, data.network_time, data.time, data.data.clone()) {
                tracing::debug!(session = sub.id, "write: {e}");
            }
            if newly_bound {
                want_keyframe = true;
            }
        }
        if want_keyframe {
            self.request_keyframe(from_id, data.mid, KeyframeRequestKind::Pli);
        }
    }

    fn request_keyframe(&mut self, publisher_id: u64, mid: Mid, kind: KeyframeRequestKind) {
        let Some(p) = self.clients.iter_mut().find(|c| c.id == publisher_id) else {
            return;
        };
        if let Some(mut writer) = p.rtc.writer(mid) {
            if let Err(e) = writer.request_keyframe(None, kind) {
                tracing::debug!(session = publisher_id, "request_keyframe: {e}");
            }
        }
    }

    // ---- socket -----------------------------------------------------------

    fn read_socket(&mut self, buf: &mut [u8]) {
        loop {
            match self.socket.recv_from(buf) {
                Ok((n, source)) => {
                    let Ok(contents) = buf[..n].try_into() else {
                        continue;
                    };
                    let input = Input::Receive(
                        Instant::now(),
                        Receive {
                            proto: Protocol::Udp,
                            source,
                            destination: self.advertised,
                            contents,
                        },
                    );
                    // str0m demuxes: ICE by STUN ufrag, SRTP by known source.
                    if let Some(client) = self.clients.iter_mut().find(|c| c.rtc.accepts(&input)) {
                        if let Err(e) = client.rtc.handle_input(input) {
                            tracing::debug!(session = client.id, "handle_input: {e}");
                        }
                    } else {
                        tracing::trace!("datagram from unknown source {source}");
                    }
                    // keep draining until WouldBlock so one slow tick can't
                    // back the socket buffer up
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                    return;
                }
                Err(e) => {
                    tracing::warn!("udp recv: {e}");
                    return;
                }
            }
        }
    }

    // ---- lifecycle --------------------------------------------------------

    fn reap_dead(&mut self) {
        let dead: Vec<u64> = self
            .clients
            .iter()
            .filter(|c| !c.rtc.is_alive())
            .map(|c| c.id)
            .collect();
        if dead.is_empty() {
            return;
        }
        self.clients.retain(|c| c.rtc.is_alive());
        for id in dead {
            tracing::info!(session = id, "session ended");
            self.publisher_gone(id);
        }
    }

    fn publisher_gone(&mut self, publisher_id: u64) {
        for c in self.clients.iter_mut() {
            c.release_publisher(publisher_id);
        }
    }
}
