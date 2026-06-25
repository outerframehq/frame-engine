use std::sync::Arc;

use frame_engine::core::Clock;
use frame_engine::systems;
use frame_engine::world::{ComponentStorage, Position, Velocity, World};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

// All the long-lived GPU objects, bundled so they travel together.
struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl GpuState {
    // wgpu's setup is async, but winit's `resumed` isn't — so we block on each
    // async call with pollster::block_on.
    fn new(window: Arc<Window>) -> GpuState {
        let size = window.inner_size();

        // 1. Instance: the entry point to wgpu.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());

        // 2. Surface: the slice of the window we draw to. Owns an Arc of the
        //    window, so it's a 'static surface.
        let surface = instance.create_surface(window.clone()).unwrap();

        // 3. Adapter: a handle to a real GPU that can draw to our surface.
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .unwrap();

        // 4. Device + Queue: the Device creates GPU resources; the Queue submits
        //    work to the card.
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();

        // 5. Configure the surface (format, size, present mode) — get_default_config
        //    fills sensible defaults so we don't hand-pick every field.
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .unwrap();
        surface.configure(&device, &config);

        GpuState {
            surface,
            device,
            queue,
            config,
        }
    }

    // Re-apply the config at a new size when the window is resized.
    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    // Draw one frame: for now, just clear the screen to a colour.
    fn render(&mut self) {
        // Acquire the texture we'll draw this frame onto. In wgpu 29 this is a
        // CurrentSurfaceTexture enum, not a Result.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // surface lost/outdated/timed-out — reconfigure and skip this frame
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };

        // A view is the handle a render pass uses to reach the texture's memory.
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // The encoder records GPU commands on the CPU side before submission.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });

        {
            // A render pass that loads by CLEARING to a colour, then stores the
            // result. With no draw calls inside, the whole frame is just the
            // clear colour. The clear happens when the pass is dropped (end of
            // this block).
            let _clear_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.12,
                            g: 0.12,
                            b: 0.16,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        // Submit the recorded commands to the GPU, then present the frame.
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
    }
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    world: World,
    paused: bool,
    clock: Clock,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes().with_title("Frame Editor");
        let window = Arc::new(event_loop.create_window(attributes).unwrap());

        // build all the GPU objects now that we have a window
        self.gpu = Some(GpuState::new(window.clone()));

        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                println!("Close requested; Shutting Down");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !event.repeat {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::Space) => {
                            self.paused = !self.paused;
                            println!("{}", if self.paused { "Paused" } else { "Playing" });
                        }
                        PhysicalKey::Code(KeyCode::KeyS) => {
                            if self.paused {
                                systems::movement(&mut self.world);
                                println!("Stepped one tick");
                            }
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // advance the sim on the shared fixed-timestep clock
                let owed = self.clock.advance(!self.paused);
                for _ in 0..owed {
                    systems::movement(&mut self.world);
                }

                if let Some(gpu) = &mut self.gpu {
                    gpu.render();
                }
                if let Some(window) = &self.window {
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
            dx: 0.4,
            dy: 0.2,
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
            dx: -0.3,
            dy: 0.3,
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
            dx: 0.5,
            dy: -0.4,
            dz: 0.0,
        },
    );

    let entity_count = world.positions.len();

    let mut app = App {
        window: None,
        gpu: None,
        world,
        paused: false,
        clock: Clock::new(TICK_RATE, MAX_CATCHUP_TICKS),
    };

    println!(
        "Editor Started, Engine World created with {} entities",
        entity_count
    );

    event_loop.run_app(&mut app).unwrap();
}
