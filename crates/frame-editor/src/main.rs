use std::sync::Arc;

use frame_engine::core::Clock;
use frame_engine::systems;
use frame_engine::world::{ComponentStorage, Position, Velocity, World};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

// Vertical field of view, shared by the projection and the pan maths.
const FOV_DEGREES: f32 = 45.0;

// Size of each entity quad in world units.
// NOTE: must match QUAD_SIZE in shader.wgsl (render and pick must agree).
const QUAD_SIZE: f32 = 8.0;

// The camera data handed to the shader. repr(C) lays the fields out in a fixed,
// predictable order; Pod + Zeroable (from bytemuck) let us copy it to the GPU as
// raw bytes. It must match the `Camera` struct in shader.wgsl.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

// Per-instance data: one entity's world position plus a selected flag (0 or 1).
// repr(C) + Pod so it copies to the GPU as raw bytes.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceRaw {
    position: [f32; 3],
    selected: f32,
}

impl InstanceRaw {
    // Two attributes: position at location 0, selected flag at location 1.
    // Stored as an associated const so the layout can hold a 'static reference.
    const ATTRIBS: [wgpu::VertexAttribute; 2] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32,
            offset: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress, // 12
            shader_location: 1,
        },
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            // Instance (not Vertex): step forward once per instance, not per vertex.
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

// Build the camera's view-projection matrix from its current state.
//   focus_x/focus_y : the world point the camera looks at (on the z = 0 plane)
//   distance        : how far back along +Z the eye sits (zoom)
fn camera_matrix(focus_x: f32, focus_y: f32, distance: f32, width: u32, height: u32) -> Mat4 {
    let aspect = width as f32 / height.max(1) as f32;

    let eye = Vec3::new(focus_x, focus_y, distance);
    let target = Vec3::new(focus_x, focus_y, 0.0);
    let up = Vec3::Y;

    let view = Mat4::look_at_rh(eye, target, up);
    let proj = Mat4::perspective_rh(FOV_DEGREES.to_radians(), aspect, 0.1, 10000.0);

    proj * view
}

// Same matrix, flattened to the column-major array the shader uniform expects.
fn camera_view_proj(focus_x: f32, focus_y: f32, distance: f32, width: u32, height: u32) -> [[f32; 4]; 4] {
    camera_matrix(focus_x, focus_y, distance, width, height).to_cols_array_2d()
}

// Project a world point through the view-projection matrix to screen pixels.
// Returns None if the point is behind the camera.
fn project(vp: Mat4, x: f32, y: f32, z: f32, width: f32, height: f32) -> Option<(f32, f32)> {
    let clip = vp * Vec4::new(x, y, z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    let screen_x = (ndc_x * 0.5 + 0.5) * width;
    let screen_y = (1.0 - (ndc_y * 0.5 + 0.5)) * height; // flip: NDC +y up -> screen +y down
    Some((screen_x, screen_y))
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

        // 5. Configure the surface (format, size, present mode).
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .unwrap();
        surface.configure(&device, &config);

        // 6. Camera uniform: start as identity; render() overwrites it every frame.
        let camera_uniform = CameraUniform {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
        };

        // 7. Upload it into a uniform buffer. COPY_DST lets render() overwrite it.
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

        // 10. Compile the shader.
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));

        // 11. Pipeline layout lists the camera bind group layout.
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout)],
            immediate_size: 0,
        });

        // 12. Build the render pipeline.
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("entity pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[InstanceRaw::layout()], // per-instance position + selected
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
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
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
        let camera_uniform = CameraUniform { view_proj };
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[camera_uniform]));

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };

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

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });

        {
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

            if let Some(buffer) = &instance_buffer {
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..6, 0..instances.len() as u32);
            }
        }

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
    // camera state
    cam_focus_x: f32,
    cam_focus_y: f32,
    cam_distance: f32,
    dragging: bool,
    last_cursor: (f64, f64),
    // selected entity id (index into world.positions)
    selected: Option<usize>,
}

impl App {
    // Project every entity to screen space and select the one under the cursor.
    fn pick(&mut self) {
        let (width_u, height_u) = match &self.window {
            Some(window) => {
                let size = window.inner_size();
                (size.width.max(1), size.height.max(1))
            }
            None => return,
        };
        let width = width_u as f32;
        let height = height_u as f32;

        let vp = camera_matrix(
            self.cam_focus_x,
            self.cam_focus_y,
            self.cam_distance,
            width_u,
            height_u,
        );

        let cursor_x = self.last_cursor.0 as f32;
        let cursor_y = self.last_cursor.1 as f32;
        let half = QUAD_SIZE * 0.5;

        let mut picked: Option<usize> = None;
        for (id, slot) in self.world.positions.iter().enumerate() {
            if let Some(p) = slot {
                // project the centre and a corner; the gap gives the on-screen half-size
                let center = project(vp, p.x, p.y, p.z, width, height);
                let corner = project(vp, p.x + half, p.y + half, p.z, width, height);
                if let (Some((cx, cy)), Some((ex, ey))) = (center, corner) {
                    let half_w = (ex - cx).abs();
                    let half_h = (ey - cy).abs();
                    if (cursor_x - cx).abs() <= half_w && (cursor_y - cy).abs() <= half_h {
                        picked = Some(id); // later entities draw on top, so last match wins
                    }
                }
            }
        }

        // Only change selection when we actually hit something — so clicking
        // empty space (or starting a pan) doesn't clear it.
        if let Some(id) = picked {
            self.selected = Some(id);
            if let (Some(p), Some(v)) =
                (self.world.positions.get(id), self.world.velocities.get(id))
            {
                println!(
                    "Selected entity {} | pos ({:.1}, {:.1}, {:.1}) | vel ({:.2}, {:.2}, {:.2})",
                    id, p.x, p.y, p.z, v.dx, v.dy, v.dz,
                );
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes().with_title("Frame Editor");
        let window = Arc::new(event_loop.create_window(attributes).unwrap());

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
                    // a press both selects (if it hits) and may begin a pan
                    if state == ElementState::Pressed {
                        self.pick();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.dragging {
                    if let Some(window) = &self.window {
                        let dx = (position.x - self.last_cursor.0) as f32;
                        let dy = (position.y - self.last_cursor.1) as f32;

                        let height_px = window.inner_size().height.max(1) as f32;
                        let visible_world_height =
                            2.0 * self.cam_distance * (FOV_DEGREES.to_radians() * 0.5).tan();
                        let world_per_px = visible_world_height / height_px;

                        self.cam_focus_x -= dx * world_per_px;
                        self.cam_focus_y += dy * world_per_px; // +Y is up on screen
                    }
                }
                self.last_cursor = (position.x, position.y);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                if scroll > 0.0 {
                    self.cam_distance /= 1.1;
                } else if scroll < 0.0 {
                    self.cam_distance *= 1.1;
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
                        PhysicalKey::Code(KeyCode::Escape) => {
                            self.selected = None;
                            println!("Selection cleared");
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let owed = self.clock.advance(!self.paused);
                for _ in 0..owed {
                    systems::movement(&mut self.world);
                }

                // build per-entity instance data, flagging the selected one
                let selected = self.selected;
                let instances: Vec<InstanceRaw> = self
                    .world
                    .positions
                    .iter()
                    .enumerate()
                    .filter_map(|(id, slot)| slot.as_ref().map(|p| (id, p)))
                    .map(|(id, p)| InstanceRaw {
                        position: [p.x, p.y, p.z],
                        selected: if Some(id) == selected { 1.0 } else { 0.0 },
                    })
                    .collect();

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
        Position { x: 0.0, y: 0.0, z: 0.0 },
        Velocity { dx: 0.4, dy: 0.2, dz: 0.0 },
    );
    world.spawn(
        Position { x: 40.0, y: 20.0, z: 0.0 },
        Velocity { dx: -0.3, dy: 0.3, dz: 0.0 },
    );
    world.spawn(
        Position { x: -30.0, y: 50.0, z: 0.0 },
        Velocity { dx: 0.5, dy: -0.4, dz: 0.0 },
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
        selected: None,
    };

    println!(
        "Editor Started, Engine World created with {} entities",
        entity_count
    );

    event_loop.run_app(&mut app).unwrap();
}
