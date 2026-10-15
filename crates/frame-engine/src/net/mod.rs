// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A first, deliberately minimal slice of multiplayer: an authoritative
//! server broadcasting entity positions to connected clients over raw TCP,
//! and clients uploading their held input back.
//!
//! # Why authoritative-server, not lockstep
//!
//! Earlier design notes here named lockstep (every client independently
//! replaying the same deterministic ticks from the same inputs, with no state
//! sent over the network at all) as the long-term aim. That fits a small,
//! closed session where every player starts and ends together, a fighting
//! game, an RTS match. It does not fit a persistent, always-on world where
//! players join and leave at any time: a late joiner would need to replay the
//! entire input history from world genesis to catch up, and any tiny
//! floating-point difference between two machines desyncs everyone
//! permanently, with no way to recover short of restarting. The engine's
//! fixed-timestep determinism stays exactly as valuable as it always was, it
//! is what makes the server's own simulation reproducible and testable, but
//! it is no longer the thing standing in for networking. Clients receive real
//! state from a server that owns the simulation, the ordinary approach for a
//! persistent world.
//!
//! # What this slice is, and deliberately is not
//!
//! - Positions sync from server to client; held input syncs from client to
//!   server. No other component syncs yet.
//! - A client is never trusted to report its own state, only its intent. It
//!   sends which of the four movement buttons are held, nothing else, the
//!   same shape `InputState` already carries for local play; the server
//!   decides what effect, if any, that has, by running the ordinary
//!   `input_movement_for` system against it. A client also never gets to
//!   choose or claim an entity id: the server spawns one for each connection
//!   as it accepts it, and remembers the pairing itself, so an upload can
//!   only ever move the entity the server already decided belongs to it.
//! - No interest management. Every call sends every entity's position to
//!   every client, a full snapshot each time. Correct at small scale, and
//!   explicitly not a solution for a large world; that is future work.
//! - No prediction or reconciliation. A client's own movement will visibly
//!   lag by however long a round trip takes: correct, but not smooth. Smoothing
//!   that out is a real, separate problem.
//! - When a client disconnects, its entity is despawned along with the
//!   connection, detected either from a closed read (in `receive_input`) or
//!   a failed write (in `broadcast_positions`), whichever notices first.
//!   Reconnecting to the same entity rather than always getting a new one on
//!   a fresh connection is still a real, separate later step.
//! - Raw TCP via `std::net`, no async runtime, no new dependency, consistent
//!   with how the rest of the engine is built. Non-blocking sockets, so a
//!   tick loop can poll without ever stalling on the network.
//! - The wire format is `serde` plus RON, reusing what the engine already
//!   depends on for scenes. Not remotely bandwidth-efficient; genuinely wrong
//!   for real traffic later, but proving correctness beats optimizing a
//!   format that is likely to change anyway once more than positions and
//!   input sync.
//! - TCP is a byte stream, not discrete messages, so every payload is framed
//!   with a 4-byte big-endian length prefix ahead of it. Even a slice this
//!   small can't skip that and still be correct; it isn't an optional nicety.
//!
//! - Versions are checked on connect. A client's first message is a
//!   `Hello` naming its `PROTOCOL`; the server answers `Welcome` (and only
//!   then spawns an entity for it) or `Refused`, naming its own version, so a
//!   mismatched pair stops with a clear reason instead of silently dropping
//!   every message neither side can read. `Hello`, `Welcome` and `Refused`
//!   keep their shape across versions (fields may only be added, with
//!   defaults), because an older build has to read them from a newer one.
//!
//! Neither `Server` nor `Client` is wired into any running process's real
//! game logic yet, only the engine binary's own debug loop; see its
//! `server`/`client` modes.

use crate::input::{Button, InputState};
use crate::systems;
use crate::world::{Controlled, Position, Velocity, World};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

/// The network protocol version. A client and server only talk if theirs
/// match. Raise it by one whenever a message changes in a way the other side
/// could misread or lose: a renamed or removed field, a changed meaning, or a
/// new field the other side must act on. A new field the other side can
/// safely ignore (with `#[serde(default)]`) does not need a raise.
///
/// 1 is the first release, `0.1.0`.
pub const PROTOCOL: u32 = 1;

/// A client's first message, before anything else: which protocol it
/// speaks, and its engine version for the error message if they differ.
/// Keeps this shape in every version (see the module notes).
#[derive(Serialize, Deserialize)]
struct Hello {
    protocol: u32,
    #[serde(default)]
    engine: String,
}

/// One entity's position, as sent over the wire. A plain, flat shape kept
/// deliberately separate from `Position` itself, so the wire format can
/// evolve (new fields, a different shape) without that also being a change
/// to the simulation's own component type.
#[derive(Serialize, Deserialize)]
struct PositionUpdate {
    id: usize,
    x: f64,
    y: f64,
    z: f64,
}

/// One broadcast message: every live entity's position, as of the moment the
/// server built it. A full snapshot, not a diff; diffing is a real
/// bandwidth optimisation for later, once this basic pipe is proven.
#[derive(Serialize, Deserialize)]
struct Snapshot {
    positions: Vec<PositionUpdate>,
}

/// Everything a `Server` can send a `Client`, wrapped in one tagged type so
/// `Client::poll` can tell them apart. `Welcome` is sent exactly once, right
/// after a client connects; `Snapshot` is sent every tick after that.
#[derive(Serialize, Deserialize)]
enum ServerMessage {
    /// Which entity is this client's own. A client has no way to claim or
    /// guess this itself; the server decides on connect and tells it once,
    /// here, rather than leaving it to infer from a snapshot alone.
    Welcome {
        entity: usize,
        #[serde(default)]
        protocol: u32,
    },
    Snapshot(Snapshot),
    /// The server will not take this client: its protocol differs, or its
    /// first message was not a `Hello` (a build from before the check).
    /// Names the server's own versions so the client can say what to do.
    Refused {
        protocol: u32,
        #[serde(default)]
        engine: String,
    },
}

/// One input message, sent client to server: which of the movement
/// buttons (and Jump, Run and Crouch) this client currently holds, and where
/// it is looking. Nothing else, deliberately; a client reports intent, never
/// its own position or velocity directly.
#[derive(Serialize, Deserialize)]
struct InputUpload {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    /// Defaults to false so a message from an older client without this
    /// field still reads.
    #[serde(default)]
    jump: bool,
    /// Run and Crouch, and where the client is looking (yaw, pitch), if it
    /// says. All default, so a message from an older client still reads.
    #[serde(default)]
    run: bool,
    #[serde(default)]
    crouch: bool,
    #[serde(default)]
    look: Option<(f32, f32)>,
    /// The actions held (`Button::Use`, `Button::UseAlt`). Default, so a
    /// message from an older client still reads.
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    secondary: bool,
}

/// Serialize `message` and write it framed. False if either step fails.
fn send<T: Serialize>(stream: &mut TcpStream, message: &T) -> bool {
    ron::ser::to_string_pretty(message, ron::ser::PrettyConfig::default())
        .ok()
        .and_then(|payload| write_framed(stream, payload.as_bytes()).ok())
        .is_some()
}

/// Take the first complete framed message off the front of `buffer`, if one
/// has fully arrived, leaving anything after it in place.
fn take_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    if buffer.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    if buffer.len() < 4 + len {
        return None;
    }
    let payload = buffer[4..4 + len].to_vec();
    buffer.drain(0..4 + len);
    Some(payload)
}

/// Read everything available on a non-blocking socket into `buffer`. True if
/// the connection has closed.
fn read_available(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> bool {
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return true,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return false,
            Err(_) => return true,
        }
    }
}

/// Write one length-prefixed message: a 4-byte big-endian length, then the
/// payload. The one piece of framing every message needs, since TCP has no
/// concept of message boundaries on its own.
fn write_framed(stream: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len)?;
    stream.write_all(payload)?;
    Ok(())
}

/// Read as many complete length-prefixed messages as `buffer` currently
/// holds, handing each one's payload bytes to `handle`. Shared by `Client`
/// (reassembling snapshots) and `Server` (reassembling input uploads), since
/// both face the exact same TCP reality: a message can arrive split across
/// several reads, or several messages can arrive in one.
fn drain_framed(buffer: &mut Vec<u8>, mut handle: impl FnMut(&[u8])) {
    loop {
        if buffer.len() < 4 {
            break; // haven't even received the length prefix yet
        }
        let len = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        if buffer.len() < 4 + len {
            break; // length is known, but the full payload isn't here yet
        }
        handle(&buffer[4..4 + len]);
        buffer.drain(0..4 + len);
    }
}

/// One connected client, from the server's side: its socket, the entity the
/// server spawned for it on connect (the only entity its input can ever
/// drive), and a buffer for reassembling input messages, the same reason
/// `Client` keeps one for position snapshots.
struct ConnectedClient {
    stream: TcpStream,
    entity: usize,
    buffer: Vec<u8>,
}

/// A connection the server has accepted but not yet welcomed: it is waiting
/// for the client's `Hello`. No entity exists for it yet.
struct PendingClient {
    stream: TcpStream,
    buffer: Vec<u8>,
}

/// The authoritative side: accepts client connections, broadcasts position
/// snapshots, and applies whatever input each client uploads to that
/// client's own entity. Owns the real simulation; a `Client` never tells it
/// what state to be in, only what buttons are held.
pub struct Server {
    listener: TcpListener,
    pending: Vec<PendingClient>,
    clients: Vec<ConnectedClient>,
}

impl Server {
    /// Bind to `addr` (for example `"0.0.0.0:7777"`) and start listening.
    /// Non-blocking: `accept_new_clients` can be polled every tick without
    /// ever stalling the loop waiting for a connection that hasn't arrived.
    pub fn bind(addr: &str) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Server {
            listener,
            pending: Vec::new(),
            clients: Vec::new(),
        })
    }

    /// The address actually bound, useful after binding port 0.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept every client that has connected since the last call, and
    /// answer any waiting client whose `Hello` has arrived. Meant to be
    /// called once per tick; never blocks. A client with the same
    /// `PROTOCOL` gets a fresh `Controlled` entity spawned for it right here,
    /// on the server (a client has no way to claim or request an id of its
    /// own), and a `Welcome` naming it. Any other client gets `Refused` with
    /// this server's versions and is dropped, without an entity.
    pub fn accept_new_clients(&mut self, world: &mut World) {
        loop {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    // A client that can't be set non-blocking is dropped
                    // rather than kept in a state that could stall the
                    // server; better to lose one bad connection than risk
                    // the whole broadcast loop hanging on it.
                    if stream.set_nonblocking(true).is_ok() {
                        self.pending.push(PendingClient {
                            stream,
                            buffer: Vec::new(),
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        let mut still_waiting = Vec::new();
        for mut pending in std::mem::take(&mut self.pending) {
            let closed = read_available(&mut pending.stream, &mut pending.buffer);
            let Some(first) = take_frame(&mut pending.buffer) else {
                if !closed {
                    still_waiting.push(pending);
                }
                continue;
            };
            let hello = std::str::from_utf8(&first)
                .ok()
                .and_then(|text| ron::from_str::<Hello>(text).ok());
            let mut stream = pending.stream;
            if hello.is_none_or(|h| h.protocol != PROTOCOL) {
                let refused = ServerMessage::Refused {
                    protocol: PROTOCOL,
                    engine: crate::version::ENGINE.to_string(),
                };
                let _ = send(&mut stream, &refused);
                continue; // dropped, which closes the connection
            }
            let entity = world.spawn(
                Position {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                Velocity {
                    dx: 0.0,
                    dy: 0.0,
                    dz: 0.0,
                },
            );
            world.controlled.insert(entity, Controlled);
            // Sent once, so the client knows which entity is its own from
            // the start rather than guessing from a snapshot. If this write
            // fails the client is dropped here too.
            let welcome = ServerMessage::Welcome {
                entity,
                protocol: PROTOCOL,
            };
            if send(&mut stream, &welcome) {
                // Input the client sent straight after its Hello is kept.
                self.clients.push(ConnectedClient {
                    stream,
                    entity,
                    buffer: pending.buffer,
                });
            } else {
                world.despawn(entity);
            }
        }
        self.pending = still_waiting;
    }

    /// Send every live entity's current position to every connected client,
    /// as one full snapshot. Meant to be called once per tick, after the
    /// world has been advanced. A client whose write fails (disconnected,
    /// buffer full past what a slice this size handles) is dropped from the
    /// list, and the entity the server spawned for it on connect is
    /// despawned along with it, so a disconnect no longer leaves an orphaned
    /// entity behind forever. A real reconnect story, letting a client
    /// resume the same entity rather than always getting a new one, is still
    /// a later step, not solved here.
    pub fn broadcast_positions(&mut self, world: &mut World) {
        let positions: Vec<PositionUpdate> = world
            .positions
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| {
                slot.as_ref().map(|p| PositionUpdate {
                    id,
                    x: p.x,
                    y: p.y,
                    z: p.z,
                })
            })
            .collect();
        let snapshot = Snapshot { positions };
        // Reuses the exact serialization call save_to_file already proves
        // works in this codebase, rather than a different, unverified one.
        // Pretty-printing is real, needless overhead on network traffic; a
        // compact format is a genuine future optimisation, deliberately not
        // solved here, correctness matters more than bandwidth for a first
        // slice proving the pipe works at all.
        let message = ServerMessage::Snapshot(snapshot);
        let Ok(payload) = ron::ser::to_string_pretty(&message, ron::ser::PrettyConfig::default())
        else {
            return;
        };
        self.clients.retain_mut(|client| {
            let ok = write_framed(&mut client.stream, payload.as_bytes()).is_ok();
            if !ok {
                world.despawn(client.entity);
            }
            ok
        });
    }

    /// Read whatever input each connected client has sent since the last
    /// call, and apply it to that client's own entity, and only that entity.
    /// There is nothing in the message a client sends that could name a
    /// different one; the pairing was fixed once, on connect, by
    /// `accept_new_clients`, and never changes. Meant to be called once per
    /// tick, same as the other two methods here.
    ///
    /// A client whose connection has closed (a clean read of zero bytes, or
    /// any other read error) is removed here and its entity despawned right
    /// away, rather than waiting for a write to it to eventually fail in
    /// `broadcast_positions`. That write-side check still exists too, as a
    /// second detection path for a connection that goes bad without the read
    /// side ever seeing it happen first.
    pub fn receive_input(&mut self, world: &mut World) {
        let mut disconnected: Vec<usize> = Vec::new();
        for (i, client) in self.clients.iter_mut().enumerate() {
            let mut chunk = [0u8; 4096];
            let mut closed = false;
            loop {
                match client.stream.read(&mut chunk) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => client.buffer.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        closed = true;
                        break;
                    }
                }
            }
            if closed {
                disconnected.push(i);
                continue; // nothing left to read input for on a dead connection
            }
            let entity = client.entity;
            drain_framed(&mut client.buffer, |payload| {
                let Ok(text) = std::str::from_utf8(payload) else {
                    return;
                };
                let Ok(upload) = ron::from_str::<InputUpload>(text) else {
                    return;
                };
                let mut input = InputState::new();
                input.set(Button::Up, upload.up);
                input.set(Button::Down, upload.down);
                input.set(Button::Left, upload.left);
                input.set(Button::Right, upload.right);
                input.set(Button::Jump, upload.jump);
                input.set(Button::Run, upload.run);
                input.set(Button::Crouch, upload.crouch);
                input.set(Button::Use, upload.primary);
                input.set(Button::UseAlt, upload.secondary);
                if let Some((yaw, pitch)) = upload.look {
                    input.set_look(yaw, pitch);
                }
                systems::input_movement_for(world, entity, &input);
                systems::input_actions_for(world, entity, &input);
            });
        }
        // Removed back to front, so an earlier index is never invalidated by
        // removing a later one first.
        for i in disconnected.into_iter().rev() {
            let client = self.clients.remove(i);
            world.despawn(client.entity);
        }
    }

    /// How many clients are currently connected, for a host to report or log.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }
}

/// The receiving side: connects to a `Server`, applies whatever position
/// snapshots arrive into a local `World`, and can upload this client's own
/// held input back. Never reports its own position or velocity, only which
/// buttons are held; the server decides what that means.
pub struct Client {
    stream: TcpStream,
    // Bytes received so far for the message currently being assembled: the
    // length prefix first, then the payload once the length is known. TCP
    // can hand back a read in any size, so a message can easily arrive
    // split across several calls to poll, or several messages can arrive in
    // one call; this buffer is what lets both be handled correctly.
    buffer: Vec<u8>,
    // The entity the server said is this client's own, learned from its
    // one-time Welcome message. None until that message has actually
    // arrived, which may take a poll or two after connecting.
    own_entity: Option<usize>,
    // Why the server would not take this client, if it refused.
    refused: Option<String>,
}

impl Client {
    /// Connect to a server at `addr`. Non-blocking, so `poll` can be called
    /// every tick without ever stalling on the network.
    /// Sends a `Hello` with this build's `PROTOCOL` first; the server
    /// answers on a later `poll`.
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let mut stream = TcpStream::connect(addr)?;
        let hello = Hello {
            protocol: PROTOCOL,
            engine: crate::version::ENGINE.to_string(),
        };
        if !send(&mut stream, &hello) {
            return Err(std::io::Error::other("could not send hello"));
        }
        stream.set_nonblocking(true)?;
        Ok(Client {
            stream,
            buffer: Vec::new(),
            own_entity: None,
            refused: None,
        })
    }

    /// Why the server would not take this client, once it has said so: a
    /// message naming both versions, ready to show. A refused client sends
    /// nothing more.
    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    /// Which entity the server said is this client's own, once its Welcome
    /// message has arrived (may be `None` for the first poll or two right
    /// after connecting, before that message has been received).
    pub fn own_entity(&self) -> Option<usize> {
        self.own_entity
    }

    /// Send this tick's held input to the server. The client only ever
    /// reports intent, which buttons are held and where it looks, never a
    /// position or velocity of its own; the server decides what effect, if
    /// any, that has, and which entity it applies to. Does nothing if the
    /// input can't be serialized, the same silent-skip already used
    /// elsewhere in this module rather than a new error path.
    pub fn send_input(&mut self, input: &InputState) {
        if self.refused.is_some() {
            return;
        }
        let upload = InputUpload {
            up: input.is_held(Button::Up),
            down: input.is_held(Button::Down),
            left: input.is_held(Button::Left),
            right: input.is_held(Button::Right),
            jump: input.is_held(Button::Jump),
            run: input.is_held(Button::Run),
            crouch: input.is_held(Button::Crouch),
            look: input.look(),
            primary: input.is_held(Button::Use),
            secondary: input.is_held(Button::UseAlt),
        };
        let Ok(payload) = ron::ser::to_string_pretty(&upload, ron::ser::PrettyConfig::default())
        else {
            return;
        };
        let _ = write_framed(&mut self.stream, payload.as_bytes());
    }

    /// Read whatever the server has sent since the last call, and apply any
    /// complete snapshot found to `world`: each entity named in it has its
    /// position overwritten with the one the snapshot carries. Meant to be
    /// called once per tick; never blocks, and does nothing if a message has
    /// only partially arrived so far, it simply waits for the rest on a
    /// later call.
    pub fn poll(&mut self, world: &mut World) {
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => break, // connection closed by the server
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        // A burst of several snapshots arriving in one read is applied in
        // order, oldest first, same as if each had arrived on its own tick.
        let own_entity = &mut self.own_entity;
        let refused = &mut self.refused;
        drain_framed(&mut self.buffer, |payload| {
            let Ok(text) = std::str::from_utf8(payload) else {
                return;
            };
            let Ok(message) = ron::from_str::<ServerMessage>(text) else {
                return;
            };
            match message {
                ServerMessage::Welcome { entity, protocol } => {
                    if protocol == PROTOCOL {
                        *own_entity = Some(entity);
                    } else {
                        *refused = Some(mismatch(protocol, "unknown"));
                    }
                }
                ServerMessage::Refused { protocol, engine } => {
                    *refused = Some(mismatch(protocol, &engine));
                }
                ServerMessage::Snapshot(snapshot) => {
                    for update in snapshot.positions {
                        world.positions.insert(
                            update.id,
                            Position {
                                x: update.x,
                                y: update.y,
                                z: update.z,
                            },
                        );
                    }
                }
            }
        });
    }
}

/// The message for a client and server that can't talk.
fn mismatch(server_protocol: u32, server_engine: &str) -> String {
    let server_engine = if server_engine.is_empty() {
        "unknown"
    } else {
        server_engine
    };
    format!(
        "The server runs a different version of Frame Engine (server {server_engine}, network protocol {server_protocol}; this is {}, protocol {PROTOCOL}). Use the same version on both.",
        crate::version::ENGINE
    )
}

#[cfg(test)]
mod handshake_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn server() -> (Server, String) {
        let server = Server::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap().to_string();
        (server, addr)
    }

    /// Read one framed message from a blocking stream.
    fn read_one(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).unwrap();
        let mut payload = vec![0u8; u32::from_be_bytes(len) as usize];
        stream.read_exact(&mut payload).unwrap();
        String::from_utf8(payload).unwrap()
    }

    #[test]
    fn a_matching_client_is_welcomed_and_gets_an_entity() {
        let (mut server, addr) = server();
        let mut world = World::new();
        let mut client = Client::connect(&addr).unwrap();
        let mut client_world = World::new();
        let start = Instant::now();
        while client.own_entity().is_none() && start.elapsed() < Duration::from_secs(5) {
            server.accept_new_clients(&mut world);
            client.poll(&mut client_world);
            std::thread::sleep(Duration::from_millis(5));
        }
        let own = client.own_entity().expect("welcomed");
        assert!(client.refused().is_none());
        assert_eq!(server.client_count(), 1);
        assert!(world.controlled.get(own).is_some());
    }

    #[test]
    fn a_client_with_another_protocol_is_refused_without_an_entity() {
        let (mut server, addr) = server();
        let mut world = World::new();
        let mut raw = TcpStream::connect(&addr).unwrap();
        let hello = Hello {
            protocol: PROTOCOL + 1,
            engine: "9.9.9".to_string(),
        };
        assert!(send(&mut raw, &hello));
        let start = Instant::now();
        let mut reply = None;
        raw.set_nonblocking(true).unwrap();
        let mut buffer = Vec::new();
        while reply.is_none() && start.elapsed() < Duration::from_secs(5) {
            server.accept_new_clients(&mut world);
            read_available(&mut raw, &mut buffer);
            reply = take_frame(&mut buffer);
            std::thread::sleep(Duration::from_millis(5));
        }
        let reply = String::from_utf8(reply.expect("answered")).unwrap();
        let message: ServerMessage = ron::from_str(&reply).unwrap();
        assert!(matches!(message, ServerMessage::Refused { protocol, .. } if protocol == PROTOCOL));
        assert_eq!(server.client_count(), 0);
        assert_eq!(world.positions.iter().flatten().count(), 0);
    }

    #[test]
    fn a_client_that_skips_the_hello_is_refused() {
        // A build from before the check sends input straight away.
        let (mut server, addr) = server();
        let mut world = World::new();
        let mut raw = TcpStream::connect(&addr).unwrap();
        write_framed(
            &mut raw,
            b"(up: true, down: false, left: false, right: false)",
        )
        .unwrap();
        raw.set_nonblocking(true).unwrap();
        let mut buffer = Vec::new();
        let mut reply = None;
        let start = Instant::now();
        while reply.is_none() && start.elapsed() < Duration::from_secs(5) {
            server.accept_new_clients(&mut world);
            read_available(&mut raw, &mut buffer);
            reply = take_frame(&mut buffer);
            std::thread::sleep(Duration::from_millis(5));
        }
        let reply = String::from_utf8(reply.expect("answered")).unwrap();
        let message: ServerMessage = ron::from_str(&reply).unwrap();
        assert!(matches!(message, ServerMessage::Refused { .. }));
        assert_eq!(server.client_count(), 0);
    }

    #[test]
    fn a_refused_client_says_why() {
        // A newer server, faked: it refuses whatever connects.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let mut client = Client::connect(&addr).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let hello: Hello = ron::from_str(&read_one(&mut stream)).unwrap();
        assert_eq!(hello.protocol, PROTOCOL);
        assert_eq!(hello.engine, crate::version::ENGINE);
        let refused = ServerMessage::Refused {
            protocol: PROTOCOL + 1,
            engine: "9.9.9".to_string(),
        };
        assert!(send(&mut stream, &refused));
        let mut world = World::new();
        let start = Instant::now();
        while client.refused().is_none() && start.elapsed() < Duration::from_secs(5) {
            client.poll(&mut world);
            std::thread::sleep(Duration::from_millis(5));
        }
        let why = client.refused().expect("refused");
        assert!(why.contains("server 9.9.9"), "{why}");
        assert!(why.contains("Use the same version"), "{why}");
        assert!(client.own_entity().is_none());
    }

    #[test]
    fn handshake_messages_keep_reading_with_fields_added_later() {
        // What a newer build might send: the same messages with more in them.
        let hello: Hello =
            ron::from_str("(protocol: 7, engine: \"2.0.0\", region: \"eu\")").unwrap();
        assert_eq!(hello.protocol, 7);
        let refused: ServerMessage =
            ron::from_str("Refused(protocol: 7, engine: \"2.0.0\", motd: \"hi\")").unwrap();
        assert!(matches!(
            refused,
            ServerMessage::Refused { protocol: 7, .. }
        ));
        let welcome: ServerMessage =
            ron::from_str("Welcome(entity: 3, protocol: 1, spawn_hint: 2)").unwrap();
        assert!(matches!(
            welcome,
            ServerMessage::Welcome {
                entity: 3,
                protocol: 1
            }
        ));
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;

    #[test]
    fn an_older_clients_input_without_actions_still_reads() {
        let old = "(up: true, down: false, left: false, right: false, jump: false, run: true)";
        let upload: InputUpload = ron::from_str(old).unwrap();
        assert!(upload.up && upload.run && !upload.primary && !upload.secondary);
        let new = "(up: false, down: false, left: false, right: false, primary: true)";
        let upload: InputUpload = ron::from_str(new).unwrap();
        assert!(upload.primary && !upload.secondary);
    }
}
