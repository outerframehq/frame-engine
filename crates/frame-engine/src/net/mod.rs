//! A first, deliberately minimal slice of multiplayer: an authoritative server
//! broadcasting entity positions to connected clients over raw TCP.
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
//! # What this first slice is, and deliberately is not
//!
//! - Positions only. No other component syncs yet; proving the pipe works
//!   end to end matters more right now than covering everything.
//! - One direction: the server broadcasts, clients only receive. Client input
//!   upload is a real, separate later step, and a genuine design question in
//!   its own right (how much a server should trust a client's own reports).
//! - No interest management. Every call sends every entity's position to
//!   every client, a full snapshot each time. Correct at small scale, and
//!   explicitly not a solution for a large world; that is future work.
//! - Raw TCP via `std::net`, no async runtime, no new dependency, consistent
//!   with how the rest of the engine is built. Non-blocking sockets, so a
//!   tick loop can poll without ever stalling on the network.
//! - The wire format is `serde` plus RON, reusing what the engine already
//!   depends on for scenes. Not remotely bandwidth-efficient; genuinely wrong
//!   for real traffic later, but proving correctness beats optimizing a
//!   format that is likely to change anyway once more than positions sync.
//! - TCP is a byte stream, not discrete messages, so every payload is framed
//!   with a 4-byte big-endian length prefix ahead of it. Even a slice this
//!   small can't skip that and still be correct; it isn't an optional nicety.
//!
//! Neither `Server` nor `Client` is wired into any running process yet. That
//! is the next step, once there is something to actually connect to.

use crate::world::{Position, World};
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

/// Write one length-prefixed message: a 4-byte big-endian length, then the
/// payload. The one piece of framing every message needs, since TCP has no
/// concept of message boundaries on its own.
fn write_framed(stream: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len)?;
    stream.write_all(payload)?;
    Ok(())
}

/// The authoritative side: accepts client connections and broadcasts position
/// snapshots. Owns the real simulation; a `Client` only ever receives from
/// it, never the reverse, in this first slice.
pub struct Server {
    listener: TcpListener,
    clients: Vec<TcpStream>,
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
    /// nothing to do this tick.
    pub fn accept_new_clients(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    // A client that can't be set non-blocking is dropped
                    // rather than kept in a state that could stall the
                    // server; better to lose one bad connection than risk
                    // the whole broadcast loop hanging on it.
                    if stream.set_nonblocking(true).is_ok() {
                        self.clients.push(stream);
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
    /// buffer full past what a slice this size handles) is dropped rather
    /// than allowed to stall the broadcast for everyone else; a real
    /// reconnect story is a later step, not solved here.
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
            .retain_mut(|stream| write_framed(stream, payload.as_bytes()).is_ok());
    }

    /// How many clients are currently connected, for a host to report or log.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }
}

/// The receiving side: connects to a `Server` and applies whatever position
/// snapshots arrive into a local `World`. Never sends anything back to the
/// server in this first slice.
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
        // Drain as many complete messages as the buffer currently holds; a
        // burst of several snapshots arriving in one read is applied in
        // order, oldest first, same as if each had arrived on its own tick.
        loop {
            if self.buffer.len() < 4 {
                break; // haven't even received the length prefix yet
            }
            let len = u32::from_be_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]) as usize;
            if self.buffer.len() < 4 + len {
                break; // length is known, but the full payload isn't here yet
            }
            let payload = &self.buffer[4..4 + len];
            if let Ok(text) = std::str::from_utf8(payload) {
                if let Ok(snapshot) = ron::from_str::<Snapshot>(text) {
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
            self.buffer.drain(0..4 + len);
        }
    }
}
