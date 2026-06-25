use std::sync::Arc;

use frame_engine::core::Clock;
use frame_engine::systems;
use frame_engine::world::{ComponentStorage, Position, Velocity, World};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

// Vertical field of view, shared by the projection and the pan maths so they
// stay in sync.
const FOV_DEGREES: f32 = 45.0;

// The camera data handed to the shader. repr(C) lays the fields out in a fixed,
// predictable order; Pod + Zeroable (from bytemuck) let us copy it to the GPU as
// raw bytes. It must match the `Camera` struct in shader.wgsl.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

// One entity's world position, uploaded as per-instance data. The vertex shader
// reads it (at @location(0)) and offsets the quad's corners to put the quad
// there. repr(C) + Pod so it copies to the GPU as raw bytes.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceRaw {
    position: [f32; 3],
}

impl InstanceRaw {
    // The one attribute: a 3-float position at shader_location 0. Stored as an
    // associated const so the layout below can hold a 'static reference to it.
    const ATTRIBS: [wgpu::VertexAttribute; 1] = [wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    }];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            // Instance (not Vertex): step forward once per instance, not per vertex.
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

// Build the combined view-projection matrix for the camera's current state.
//
//   focus_x/focus_y : the world point the camera looks at (on the z = 0 plane)
//   distance        : how far back along +Z the eye sits (zoom)
//
// perspective_rh targets wgpu's [0,1] depth range directly. The far plane is
// large enough to cover the full zoom-out range.
fn camera_view_proj(
    focus_x: f32,
    focus_y: f32,
    distance: f32,
    width: u32,
    height: u32,
) -> [[f32; 4]; 4] {
    let aspect = width as f32 / height.max(1) as f32;

    let eye = Vec3::new(focus_x, focus_y, distance);
    let target = Vec3::new(focus_x, focus_y, 0.0);
    let up = Vec3::Y;

    let view = Mat4::look_at_rh(eye, target, up);
    let proj = Mat4::perspective_rh(FOV_DEGREES.to_radians(), aspect, 0.1, 10000.0);

    (proj * view).to_cols_array_2d()
}

// All the long-lived GPU objects, bundled so they travel together.
struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    render_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
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

        // 6. Camera uniform: start as identity; render() overwrites it every
        //    frame from the live camera state.
        let camera_uniform = CameraUniform {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
        };

        // 7. Upload it into a uniform buffer on the GPU. COPY_DST lets render()
        //    overwrite it each frame.
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera buffer"),
            contents: bytemuck::cast_slice(&[camera_uniform]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // 8. The contract: binding 0 is a uniform buffer the vertex shader reads.
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("camera bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        // 9. The wiring: this specific buffer fills binding 0 of that contract.
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        // 10. Compile the shader (vertex + fragment stages, from shader.wgsl).
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));

        // 11. Pipeline layout lists the camera bind group layout.
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout)],
            immediate_size: 0,
        });

        // 12. Build the render pipeline. The vertex stage reads one buffer: the
        //     per-instance entity positions.
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("entity pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[InstanceRaw::layout()], // per-instance positions
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(), // TriangleList, no culling
            depth_stencil: None,                        // no depth buffer yet (step 5)
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        GpuState {
            surface,
            device,
            queue,
            config,
            render_pipeline,
            camera_buffer,
            camera_bind_group,
        }
    }

    // Re-apply the config at a new size. The camera matrix is rebuilt every
    // frame in render(), so there's nothing camera-related to do here.
    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    // Draw one frame: upload the current camera matrix, clear, then draw one
    // quad per entity.
    fn render(&mut self, instances: &[InstanceRaw], view_proj: [[f32; 4]; 4]) {
        // Upload this frame's camera matrix.
        let camera_uniform = CameraUniform { view_proj };
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&[camera_uniform]),
        );

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

        // Rebuild the instance buffer from this frame's entity positions. At a
        // few entities, recreating it each frame is trivially cheap; we'll switch
        // to a persistent buffer when that cost is real. Skip it when empty —
        // a zero-sized buffer is invalid.
        let instance_buffer = if instances.is_empty() {
            None
        } else {
            Some(
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("instance buffer"),
                        contents: bytemuck::cast_slice(instances),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
            )
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
            // The pass clears to the background colour, then we draw into it.
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main pass"),
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

            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);

            // Draw the quad (6 vertices) once per entity instance.
            if let Some(buffer) = &instance_buffer {
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..6, 0..instances.len() as u32);
            }
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
    // camera state (lives here because this is where the mouse events arrive)
    cam_focus_x: f32,
    cam_focus_y: f32,
    cam_distance: f32,
    dragging: bool,
    last_cursor: (f64, f64),
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
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    self.dragging = state == ElementState::Pressed;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.dragging {
                    if let Some(window) = &self.window {
                        // pixel delta since the last cursor report
                        let dx = (position.x - self.last_cursor.0) as f32;
                        let dy = (position.y - self.last_cursor.1) as f32;

                        // how much world a pixel covers, at the current distance
                        let height_px = window.inner_size().height.max(1) as f32;
                        let visible_world_height =
                            2.0 * self.cam_distance * (FOV_DEGREES.to_radians() * 0.5).tan();
                        let world_per_px = visible_world_height / height_px;

                        // grab-and-drag: move the focus so the world follows the cursor
                        self.cam_focus_x -= dx * world_per_px;
                        self.cam_focus_y += dy * world_per_px; // +Y is up on screen
                    }
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
                    self.cam_distance /= 1.1; // scroll up = closer = zoom in
                } else if scroll < 0.0 {
                    self.cam_distance *= 1.1; // scroll down = further = zoom out
                }
                self.cam_distance = self.cam_distance.clamp(10.0, 2000.0);
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

                // build per-entity instance data from the current world state
                let instances: Vec<InstanceRaw> = self
                    .world
                    .positions
                    .iter()
                    .filter_map(|slot| slot.as_ref())
                    .map(|p| InstanceRaw {
                        position: [p.x, p.y, p.z],
                    })
                    .collect();

                // window size, for the camera's aspect ratio
                let (width, height) = match &self.window {
                    Some(window) => {
                        let size = window.inner_size();
                        (size.width, size.height)
                    }
                    None => (1, 1),
                };

                let view_proj = camera_view_proj(
                    self.cam_focus_x,
                    self.cam_focus_y,
                    self.cam_distance,
                    width,
                    height,
                );

                if let Some(gpu) = &mut self.gpu {
                    gpu.render(&instances, view_proj);
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
        cam_focus_x: 0.0,
        cam_focus_y: 0.0,
        cam_distance: 150.0,
        dragging: false,
        last_cursor: (0.0, 0.0),
    };

    println!(
        "Editor Started, Engine World created with {} entities",
        entity_count
    );

    event_loop.run_app(&mut app).unwrap();
}
