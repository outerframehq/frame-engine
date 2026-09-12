use frame_engine::core::Clock;
use frame_engine::input::{Button, InputState};
use frame_engine::net::{Client, Server};
use frame_engine::world::{Position, Velocity, World};
use frame_engine::{render, systems};

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

/// `cargo run -p frame-engine` with no arguments keeps the original,
/// no-networking behaviour exactly as it was. `server` and `client` are the
/// new modes, the first real use of `net::Server`/`net::Client`: run one
/// instance as `server`, another as `client`, and a position broadcast
/// crossing the network, and a client's input reaching the server and
/// actually moving something, both become things you can watch happen in
/// two terminals rather than code sitting unused in the source tree.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("server") => {
            let addr = args.get(2).map(String::as_str).unwrap_or("0.0.0.0:7777");
            run_server(addr);
        }
        Some("client") => {
            let addr = args.get(2).map(String::as_str).unwrap_or("127.0.0.1:7777");
            run_client(addr);
        }
        _ => run_standalone(),
    }
}

/// The four spawned entities that standalone and server mode both start
/// from, pulled out once so the same setup isn't duplicated between them.
/// The client starts from an empty world instead; it has no simulation of
/// its own, only whatever a server sends it.
fn default_world() -> World {
    let mut world = World::new();
    world.spawn(
        Position {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        Velocity {
            dx: 1.0,
            dy: 0.0,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: 10.0,
            y: 5.0,
            z: 1.0,
        },
        Velocity {
            dx: 0.0,
            dy: 1.0,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: -3.0,
            y: 2.0,
            z: 1.0,
        },
        Velocity {
            dx: 1.0,
            dy: 1.0,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: 5.0,
            y: 2.0,
            z: 2.0,
        },
        Velocity {
            dx: 1.0,
            dy: 1.0,
            dz: 0.0,
        },
    );
    world
}

/// The original behaviour: a handful of moving entities, ticked and drawn
/// locally, no server or client involved. What running the binary with no
/// arguments still does, unchanged.
fn run_standalone() {
    println!("Frame Engine starting up.");

    let mut clock = Clock::new(TICK_RATE, MAX_CATCHUP_TICKS);
    let mut tick: u64 = 0;
    let mut world = default_world();

    loop {
        // ask the shared clock how many fixed ticks are owed, then run each.
        // the engine always runs, so we pass `true`.
        let owed = clock.advance(true);
        for _ in 0..owed {
            tick += 1; // add on to tick count
            systems::movement(&mut world); // run the movement system
            if tick % 6 == 0 {
                println!("Tick {tick}");
                render::debug_print(&world);
            }
        }
    }
}

/// Run as the authoritative side: owns the real simulation, accepts client
/// connections (spawning a fresh `Controlled` entity for each one), applies
/// whatever input each client uploads to that client's own entity, and
/// broadcasts every entity's position to whichever clients are connected.
fn run_server(addr: &str) {
    println!("Frame Engine starting as a server, listening on {addr}.");

    let mut server = match Server::bind(addr) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("Could not bind to {addr}: {e}");
            return;
        }
    };

    let mut clock = Clock::new(TICK_RATE, MAX_CATCHUP_TICKS);
    let mut tick: u64 = 0;
    let mut world = default_world();

    loop {
        server.accept_new_clients(&mut world);
        server.receive_input(&mut world);
        let owed = clock.advance(true);
        for _ in 0..owed {
            tick += 1;
            systems::movement(&mut world);
        }
        server.broadcast_positions(&mut world);
        if tick % 30 == 0 {
            println!(
                "Tick {tick}, {} client(s) connected.",
                server.client_count()
            );
        }
    }
}

/// Run as a receiving client: no simulation of its own, just whatever a
/// server broadcasts, applied and occasionally printed so a position update
/// arriving over the network is something you can actually watch happen.
/// Also uploads held input every tick, so the other real half of this
/// slice, a client's input actually reaching the server and moving
/// something, is exercised too, not just the broadcast direction.
///
/// There is no real keyboard here, this is a headless debug binary, so the
/// input is a fixed, fake pattern (holding Right the whole time) rather than
/// anything interactive: enough to prove the pipe moves an entity, not a
/// stand-in for real play. The clock paces the printing only; nothing in
/// this mode advances the world locally between polls.
fn run_client(addr: &str) {
    println!("Frame Engine connecting as a client to {addr}.");

    let mut client = match Client::connect(addr) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Could not connect to {addr}: {e}");
            return;
        }
    };

    let mut clock = Clock::new(TICK_RATE, MAX_CATCHUP_TICKS);
    let mut tick: u64 = 0;
    let mut world = World::new();
    let mut input = InputState::new();
    input.set(Button::Right, true);

    loop {
        client.poll(&mut world);
        client.send_input(&input);
        let owed = clock.advance(true);
        for _ in 0..owed {
            tick += 1;
            if tick % 30 == 0 {
                println!("Tick {tick}");
                render::debug_print(&world);
            }
        }
    }
}
