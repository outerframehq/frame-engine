use frame_engine::world::World;
use frame_engine::world::{ComponentStorage, Position, Velocity};
use std::num::NonZeroU32;
use std::rc::Rc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

struct App {
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    world: World,
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
            WindowEvent::RedrawRequested => {
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
                        let entity_color: u32 = (220 << 16) | (220 << 8) | 80; //warm yellow
                        let orgin_x = width_px / 2;
                        let orgin_y = height_px / 2;

                        for slot in self.world.positions.iter() {
                            if let Some(position) = slot {
                                // map wold x/y to a pixel position
                                let px = orgin_x as i32 + position.x as i32;
                                let py = orgin_y as i32 + position.y as i32;

                                let size: i32 = 6; // entity dot size in pixels

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
                            }
                        }

                        buffer.present().unwrap();
                    }
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
            dx: 0.0,
            dy: 0.0,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: 40.0,
            y: 20.0,
            z: 0.0,
        },
        Velocity {
            dx: 0.0,
            dy: 0.0,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: -30.0,
            y: 50.0,
            z: 0.0,
        },
        Velocity {
            dx: 0.0,
            dy: 0.0,
            dz: 0.0,
        },
    );

    let entity_count = world.positions.len();

    let mut app = App {
        window: None,
        surface: None,
        world,
    };

    println!(
        "Editor Started, Engine World created with {} entities",
        entity_count
    );

    event_loop.run_app(&mut app).unwrap();
}
