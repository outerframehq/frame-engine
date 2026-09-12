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
//! - A client still has no way to learn which entity in a broadcast is its
//!   own; it can drive it, but can't yet tell which dot on screen is
//!   "itself" from the snapshot alone. A real future step, not solved here.
//! - When a client disconnects, its entity is not despawned; it's left
//!   behind in the world. Cleaning up a dropped connection's entity, and
//!   reconnecting at all, are both real, separate later steps.
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
//! Neither `Server` nor `Client` is wired into any running process's real
//! game logic yet, only the engine binary's own debug loop; see its
//! `server`/`client` modes.

use crate::input::{Button, InputState};
use crate::systems;
use crate::world::{Controlled, Position, Velocity, World};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

/// One entity's position, as sent over the wire. A plain, flat shape kept
/// deliberately separate from `Position` itself, so the wire format can
/// evolve (new fields, a different shape) without that also being a change
/// to the simulation's own component type.
#[derive(Serialize, Deserialize)]
struct PositionUpdate {
    id: usize,
    x: f32,
    y: f32,
    z: f32,
}

/// One broadcast message: every live entity's position, as of the moment the
/// server built it. A full snapshot, not a diff; diffing is a real
/// bandwidth optimisation for later, once this basic pipe is proven.
#[derive(Serialize, Deserialize)]
struct Snapshot {
    positions: Vec<PositionUpdate>,
}

/// One input message, sent client to server: which of the four movement
/// buttons this client currently holds. Nothing else, deliberately; a
/// client reports intent, never its own position or velocity directly.
#[derive(Serialize, Deserialize)]
struct InputUpload {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
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

/// The authoritative side: accepts client connections, broadcasts position
/// snapshots, and applies whatever input each client uploads to that
/// client's own entity. Owns the real simulation; a `Client` never tells it
/// what state to be in, only what buttons are held.
pub struct Server {
    listener: TcpListener,
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
            clients: Vec::new(),
        })
    }

    /// Accept every client that has connected since the last call. Meant to
    /// be called once per tick; never blocks, since the listener is
    /// non-blocking, an absence of new connections is not an error, just
    /// nothing to do this tick. Each accepted client gets a fresh
    /// `Controlled` entity spawned for it right here, on the server: a
    /// client has no way to claim or request an id of its own.
    pub fn accept_new_clients(&mut self, world: &mut World) {
        loop {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    // A client that can't be set non-blocking is dropped
                    // rather than kept in a state that could stall the
                    // server; better to lose one bad connection than risk
                    // the whole broadcast loop hanging on it.
                    if stream.set_nonblocking(true).is_ok() {
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
                        self.clients.push(ConnectedClient {
                            stream,
                            entity,
                            buffer: Vec::new(),
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    /// Send every live entity's current position to every connected client,
    /// as one full snapshot. Meant to be called once per tick, after the
    /// world has been advanced. A client whose write fails (disconnected,
    /// buffer full past what a slice this size handles) is dropped from the
    /// list rather than allowed to stall the broadcast for everyone else;
    /// its spawned entity is deliberately left behind in the world, real
    /// cleanup on disconnect is a later step, not solved here, the same way
    /// a real reconnect story isn't either.
    pub fn broadcast_positions(&mut self, world: &World) {
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
        let Ok(payload) = ron::ser::to_string_pretty(&snapshot, ron::ser::PrettyConfig::default())
        else {
            return;
        };
        self.clients
            .retain_mut(|client| write_framed(&mut client.stream, payload.as_bytes()).is_ok());
    }

    /// Read whatever input each connected client has sent since the last
    /// call, and apply it to that client's own entity, and only that entity.
    /// There is nothing in the message a client sends that could name a
    /// different one; the pairing was fixed once, on connect, by
    /// `accept_new_clients`, and never changes. Meant to be called once per
    /// tick, same as the other two methods here.
    pub fn receive_input(&mut self, world: &mut World) {
        for client in &mut self.clients {
            let mut chunk = [0u8; 4096];
            loop {
                match client.stream.read(&mut chunk) {
                    Ok(0) => break, // connection closed by the client
                    Ok(n) => client.buffer.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
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
                systems::input_movement_for(world, entity, &input);
            });
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
}

impl Client {
    /// Connect to a server at `addr`. Non-blocking, so `poll` can be called
    /// every tick without ever stalling on the network.
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nonblocking(true)?;
        Ok(Client {
            stream,
            buffer: Vec::new(),
        })
    }

    /// Send this tick's held input to the server. The client only ever
    /// reports intent, which of the four buttons are held, never a
    /// position or velocity of its own; the server decides what effect, if
    /// any, that has, and which entity it applies to. Does nothing if the
    /// input can't be serialized, the same silent-skip already used
    /// elsewhere in this module rather than a new error path.
    pub fn send_input(&mut self, input: &InputState) {
        let upload = InputUpload {
            up: input.is_held(Button::Up),
            down: input.is_held(Button::Down),
            left: input.is_held(Button::Left),
            right: input.is_held(Button::Right),
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
        drain_framed(&mut self.buffer, |payload| {
            let Ok(text) = std::str::from_utf8(payload) else {
                return;
            };
            let Ok(snapshot) = ron::from_str::<Snapshot>(text) else {
                return;
            };
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
        });
    }
}
