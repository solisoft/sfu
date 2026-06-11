# soli-sfu

A lightweight group **SFU** (Selective Forwarding Unit) for the Soli
ecosystem, built on [str0m](https://github.com/algesten/str0m) (sans-IO
WebRTC). Designed for bonfire's 3D office: small **proximity groups** whose
media must scale past what a client-side mesh can carry (~4-6 peers), without
running a heavyweight media server.

```
browser ──HTTPS (soli-proxy)──▶ control API  (axum,  :9300)   SDP + groups
browser ──UDP, direct────────▶ media engine (str0m, :3478)   RTP/RTCP, ONE port
```

* **ICE-lite, single UDP port.** The SFU is the publicly reachable side; all
  peers are multiplexed on one socket (STUN ufrag / source-address demux).
  No TURN needed for the common case.
* **No transcoding, no simulcast, no BWE** - v1 forwards packets, period.
  Cap your bitrates client-side.
* **Group-scoped forwarding.** Each session carries a `peers` list (the
  user_ids it should hear - bonfire's proximity group). Media fans out only
  inside the group, never to the whole room.
* **Slot-based downstream, zero renegotiation.** Subscribers pre-allocate
  `audio_slots`/`video_slots` recvonly m-lines in their offer; the SFU maps
  publishers onto free slots dynamically (steals the longest-idle slot past
  3s when full). Group sizes of 2-8 never exhaust the defaults (8/4).
* **Own service on purpose.** Do NOT embed in soli-proxy: media sessions are
  long-lived UDP state and must survive proxy blue-green deployments.

## Run

```bash
cargo run -- dev.toml                     # local: unauthenticated, loopback
open examples/client.html                 # in two tabs -> they hear each other

cp config.example.toml sfu.toml           # production: set public_ip, secret
SOLI_SFU_SECRET=... cargo run --release -- sfu.toml
```

Open UDP `udp_port` in the firewall (default 3478). Front `control_addr`
with soli-proxy for TLS; never proxy the UDP port.

## Control API

| Route | Body | Response |
|---|---|---|
| `POST /v1/sessions` | `{token, sdp_offer, peers?}` | `{session_id, sdp_answer}` |
| `PATCH /v1/sessions/:id` | `{token, peers}` | 204 |
| `DELETE /v1/sessions/:id` | `{token}` | 204 |
| `GET /v1/stats` | - | `{sessions, rooms}` |
| `GET /healthz` | - | `ok` |

`peers` is the list of user_ids this subscriber should hear; `null`/omitted
means everyone in the room. Re-PATCH it whenever the conversation group
changes shape. Reconnecting (same user + room) replaces the previous session.

## Tokens

Byte-compatible with bonfire's `GatherWs` scheme so the Soli app mints them
with existing builtins (HMAC-SHA256, lowercase hex - `Crypto.hmac`):

```soli
exp     = DateTime.utc().to_unix() + 3600
payload = Base64.encode(user_id) + ":" + Base64.encode(room) + ":" + str(exp)
signed  = "sfu1." + payload
token   = signed + "." + Crypto.hmac(signed, secret)
```

The secret is `SOLI_SFU_SECRET`, falling back to `SOLI_WEBHOOK_SECRET` (so
bonfire and the SFU can share one). Dev configs may set
`allow_unauthenticated = true` to accept `dev.<user>.<room>` pseudo-tokens.

```bash
soli-sfu mint-token u1 spatial:acme 3600   # CLI mint for curl/testing
```

## Client contract

The browser builds ONE RTCPeerConnection per room:

1. `addTransceiver(micTrack, {direction:'sendonly'})` (+ one sendonly video
   transceiver, `replaceTrack` when the cam/screen starts);
2. `audio_slots` x `addTransceiver('audio', {direction:'recvonly'})` and
   `video_slots` x video - these are the downstream slots;
3. `createOffer` -> `POST /v1/sessions` -> `setRemoteDescription(answer)`.

No STUN servers needed: the answer carries the SFU's host candidate.
`examples/client.html` is the reference implementation.

## Bonfire integration sketch

* `Sfu` service in the app mints the token (`Rtc`/`GatherWs` style) and
  proxies `POST /v1/sessions` server-side (or the browser calls the SFU
  directly - the token is the auth either way).
* `gather-3d.js`: keep the mesh for groups <= 4; past that, switch the group
  to one SFU PeerConnection (room `spatial:<cid>`), and PATCH `peers` from
  `reconcileRtc` whenever the proximity group changes.
* Media-level group privacy is enforced server-side by `peers` - a locked
  circle maps to it directly.

## Limits (v1, deliberate)

* No simulcast / congestion control: fix sane client bitrates
  (`sender.setParameters` maxBitrate ~64kbps audio / 600kbps video).
* No TCP/TLS media fallback: UDP-blocked networks won't connect (add ice-tcp
  or a TURN later if it matters).
* Slots bound concurrent publishers per subscriber (defaults 8 audio /
  4 video) - matched to proximity-group sizes, not all-hands broadcasts.
* In-memory state only: an SFU restart drops live calls (clients re-join).

## Docs site

`www/` is a Soli app serving the full documentation (`soli serve www --dev`).
