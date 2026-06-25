mod font;
use frame_engine::core::Clock;
use frame_engine::systems;
use frame_engine::world::World;
use frame_engine::world::{ComponentStorage, Position, Velocity};
use std::num::NonZeroU32;
use std::rc::Rc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

struct App {
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    world: World,
    paused: bool,
    clock: Clock,
    cam_x: f32,
    cam_y: f32,
    zoom: f32,
    dragging: bool,
    last_cursor: (f64, f64),
    selected: Option<usize>,
}

impl App {
    fn pick_entity(&mut self) {
        // window centre, in physical pixels — same coordinate space as the cursor
        let (center_x, center_y) = match &self.window {
            Some(window) => {
                let size = window.inner_size();
                (size.width as f32 / 2.0, size.height as f32 / 2.0)
            }
            None => return,
        };

        let cursor_x = self.last_cursor.0 as f32;
        let cursor_y = self.last_cursor.1 as f32;

        let mut picked: Option<usize> = None;

        for (id, slot) in self.world.positions.iter().enumerate() {
            if let Some(position) = slot {
                // identical projection + size to the renderer
                let px = center_x + (position.x - self.cam_x) * self.zoom;
                let py = center_y + (position.y - self.cam_y) * self.zoom;

                let depth = position.z;
                let side = (6.0 + depth * 0.6).max(2.0);
                let half = side / 2.0;

                // exact hit: is the cursor inside this entity's square?
                if cursor_x >= px - half
                    && cursor_x < px + half
                    && cursor_y >= py - half
                    && cursor_y < py + half
                {
                    picked = Some(id); // later entities draw on top, so last match wins
                }
            }
        }

        // Only change selection when we actually hit something. That way panning
        // from empty space neither clears the selection nor spams the console.
        if let Some(id) = picked {
            self.selected = Some(id);
            if let (Some(position), Some(velocity)) =
                (self.world.positions.get(id), self.world.velocities.get(id))
            {
                println!(
                    "Selected entity {} | pos ({:.1}, {:.1}, {:.1}) | vel ({:.2}, {:.2}, {:.2})",
                    id, position.x, position.y, position.z, velocity.dx, velocity.dy, velocity.dz,
                );
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes().with_title("Frame Editor");
        let window = Rc::new(event_loop.create_window(attributes).unwrap());

        let context = softbuffer::Context::new(window.clone()).unwrap();
        let surface = softbuffer::Surface::new(&context, window.clone()).unwrap();

        window.request_redraw();

        self.window = Some(window);
        self.surface = Some(surface);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                println!("Close requested; Shutting Down");
                event_loop.exit();
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    self.dragging = state == ElementState::Pressed;
                    if state == ElementState::Pressed {
                        self.pick_entity();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.dragging {
                    // how far the cursor moved, in pixels, since last report
                    let dx = (position.x - self.last_cursor.0) as f32;
                    let dy = (position.y - self.last_cursor.1) as f32;
                    // move the camera opposite the drag, converting pixels -> world units.
                    // grab-and-drag: the world follows the cursor.
                    self.cam_x -= dx / self.zoom;
                    self.cam_y -= dy / self.zoom;
                }
                self.last_cursor = (position.x, position.y);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // wheel reports line notches; trackpads report pixels — handle both
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                if scroll > 0.0 {
                    self.zoom *= 1.1; // scroll up = zoom in
                } else if scroll < 0.0 {
                    self.zoom /= 1.1; // scroll down = zoom out
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                // only react to the initial press — not auto-repeat, not release
                if event.state == ElementState::Pressed && !event.repeat {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::F5) => {
                            match self.world.save_to_file("scene.ron") {
                                Ok(()) => println!("Saved scene to scene.ron"),
                                Err(e) => println!("Save failed: {}", e),
                            }
                        }
                        PhysicalKey::Code(KeyCode::F9) => {
                            match World::load_from_file("scene.ron") {
                                Ok(world) => {
                                    self.world = world;
                                    self.selected = None; // ids may differ in the loaded world
                                    println!("Loaded scene from scene.ron");
                                }
                                Err(e) => println!("Load failed: {}", e),
                            }
                        }
                        PhysicalKey::Code(KeyCode::Space) => {
                            self.paused = !self.paused;
                            if self.paused {
                                println!("Paused");
                            } else {
                                println!("Playing");
                            }
                        }
                        PhysicalKey::Code(KeyCode::KeyS) => {
                            if self.paused {
                                systems::movement(&mut self.world);
                                println!("Stepped one tick");
                            }
                        }
                        PhysicalKey::Code(KeyCode::Escape) => {
                            self.selected = None;
                            println!("Selection cleared");
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // ask the shared clock how many fixed ticks are owed since last
                // frame, and run the simulation that many times. Passing `!self.paused`
                // means a paused editor keeps its timing current but advances zero ticks.
                let owed = self.clock.advance(!self.paused);
                for _ in 0..owed {
                    systems::movement(&mut self.world);
                }

                if let (Some(window), Some(surface)) = (&self.window, &mut self.surface) {
                    let size = window.inner_size();

                    if let (Some(width), Some(height)) =
                        (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                    {
                        surface.resize(width, height).unwrap();

                        let width_px = width.get();
                        let height_px = height.get();

                        let mut buffer = surface.buffer_mut().unwrap();

                        // fill the background with dark blue grey
                        let background: u32 = (30 << 16) | (30 << 8) | 40; //dark blue grey
                        for pixel in buffer.iter_mut() {
                            *pixel = background;
                        }

                        // draw each entity as a bright dot
                        let base_r: f32 = 220.0;
                        let base_g: f32 = 220.0;
                        let base_b: f32 = 80.0;
                        let center_x = width_px as f32 / 2.0;
                        let center_y = height_px as f32 / 2.0;

                        for (id, slot) in self.world.positions.iter().enumerate() {
                            if let Some(position) = slot {
                                // project world position through the camera
                                let px = (center_x + (position.x - self.cam_x) * self.zoom) as i32;
                                let py = (center_y + (position.y - self.cam_y) * self.zoom) as i32;

                                //fake z dpeth modulates size and brightness
                                let depth = position.z;
                                let size = (6.0 + depth * 0.6).max(2.0) as i32;

                                let brightness = (1.0 + depth * 0.06).clamp(0.35, 1.4);
                                let r = (base_r * brightness).min(255.0) as u32;
                                let g = (base_g * brightness).min(255.0) as u32;
                                let b = (base_b * brightness).min(255.0) as u32;
                                let entity_color = (r << 16) | (g << 8) | b;

                                // draw a sizexsize square centered at px, py
                                for dy in -size / 2..size / 2 {
                                    for dx in -size / 2..size / 2 {
                                        let x = px + dx;
                                        let y = py + dy;

                                        if x >= 0
                                            && x < width_px as i32
                                            && y >= 0
                                            && y < height_px as i32
                                        {
                                            let index = y as u32 * width_px + x as u32;
                                            buffer[index as usize] = entity_color;
                                        }
                                    }
                                }
                                //only draw if it's inside the window
                                if px >= 0
                                    && px < width_px as i32
                                    && py >= 0
                                    && py < height_px as i32
                                {
                                    let index = py as u32 * width_px + px as u32;
                                    buffer[index as usize] = entity_color;
                                }

                                // highlight the selected entity with a ring
                                if Some(id) == self.selected {
                                    let ring_color: u32 = (255 << 16) | (255 << 8) | 255; // white
                                    let ring_half = size / 2 + 3; // sits a few px outside the square
                                    for dy in -ring_half..=ring_half {
                                        for dx in -ring_half..=ring_half {
                                            // border only — skip the filled interior
                                            if dx == -ring_half
                                                || dx == ring_half
                                                || dy == -ring_half
                                                || dy == ring_half
                                            {
                                                let x = px + dx;
                                                let y = py + dy;
                                                if x >= 0
                                                    && x < width_px as i32
                                                    && y >= 0
                                                    && y < height_px as i32
                                                {
                                                    let index = y as u32 * width_px + x as u32;
                                                    buffer[index as usize] = ring_color;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // draw the selected entity's data as on-screen text
                        if let Some(id) = self.selected {
                            if let (Some(position), Some(velocity)) =
                                (self.world.positions.get(id), self.world.velocities.get(id))
                            {
                                let text = format!(
                                    "ID {}\nPOS {:.1}, {:.1}, {:.1}\nVEL {:.2}, {:.2}, {:.2}",
                                    id,
                                    position.x,
                                    position.y,
                                    position.z,
                                    velocity.dx,
                                    velocity.dy,
                                    velocity.dz,
                                );
                                let text_color: u32 = (255 << 16) | (255 << 8) | 255; // white
                                font::draw_text(
                                    &mut buffer,
                                    width_px,
                                    height_px,
                                    8,
                                    8,
                                    &text,
                                    2,
                                    text_color,
                                );
                            }
                        }

                        buffer.present().unwrap();
                    }

                    // schedule the next frame so the loop keeps running
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap();

    let mut world = World {
        positions: ComponentStorage::new(),
        velocities: ComponentStorage::new(),
    };

    world.spawn(
        Position {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        Velocity {
            // dirft near = Big & bright
            dx: 0.4,
            dy: 0.2,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            //parked near = big & bright
            x: 40.0,
            y: 20.0,
            z: 0.0,
        },
        Velocity {
            dx: -0.3,
            dy: 0.3,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            // parked far = small & dim
            x: -30.0,
            y: 50.0,
            z: 0.0,
        },
        Velocity {
            dx: 0.5,
            dy: -0.4,
            dz: 0.0,
        },
    );

    let entity_count = world.positions.len();

    let mut app = App {
        window: None,
        surface: None,
        world,
        paused: false,
        clock: Clock::new(TICK_RATE, MAX_CATCHUP_TICKS),
        cam_x: 0.0,
        cam_y: 0.0,
        zoom: 1.0,
        dragging: false,
        last_cursor: (0.0, 0.0),
        selected: None,
    };

    println!(
        "Editor Started, Engine World created with {} entities",
        entity_count
    );

    event_loop.run_app(&mut app).unwrap();
}
