const LOGO_PNG: &[u8] = include_bytes!("../assets/frame-editor.png");

use std::sync::Arc;

use frame_engine::core::Clock;
use frame_engine::input::{Button, InputState};
use frame_engine::systems;
use frame_engine::world::{Controlled, Position, Velocity, World};
use glam::{Mat4, Vec3, Vec4};
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Icon, Window, WindowId};

mod font;

const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;

// Vertical field of view, shared by the projection and the pan maths.
const FOV_DEGREES: f32 = 45.0;

// Size of each entity cube in world units.
// NOTE: must match CUBE_SIZE in shader.wgsl (render and pick must agree).
const QUAD_SIZE: f32 = 8.0;

// How fast middle-drag sweeps the orbit, in radians per pixel.
const ORBIT_SENS: f32 = 0.005;

// Format of the depth buffer. 32-bit float depth, no stencil.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

// Where scenes are saved to / loaded from. Relative to the working directory,
// which is the workspace root when run via `cargo run`. Gitignored.
const SCENE_PATH: &str = "scene.ron";

// How far a single nudge moves the selected entity, in world units.
const EDIT_STEP: f32 = 5.0;

// The camera data handed to the shader. Must match the `Camera` struct in shader.wgsl.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

// Per-entity instance data: world position plus a selected flag (0 or 1).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceRaw {
    position: [f32; 3],
    color: [f32; 3],
    selected: f32,
    scale: f32,
}

impl InstanceRaw {
    const ATTRIBS: [wgpu::VertexAttribute; 4] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress, // 12
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32,
            offset: std::mem::size_of::<[f32; 6]>() as wgpu::BufferAddress, // 24
            shader_location: 2,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32,
            offset: std::mem::size_of::<[f32; 7]>() as wgpu::BufferAddress, // 28
            shader_location: 3,
        },
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

// A screen-space overlay rectangle (text pixel), positioned directly in NDC.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TextInstance {
    offset: [f32; 2], // bottom-left corner in NDC
    size: [f32; 2],   // width/height in NDC
}

impl TextInstance {
    const ATTRIBS: [wgpu::VertexAttribute; 2] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress, // 8
            shader_location: 1,
        },
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TextInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBS,
        }
    }
}

// Create (or recreate) the depth texture's view, sized to match the surface.
// Called once at startup and again on every resize.
fn create_depth_view(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth texture"),
        size: wgpu::Extent3d {
            width: config.width.max(1),
            height: config.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

// Turn a string into a pile of screen-space quads, one per lit font pixel.
//
// Coordinates flow: a font pixel lives at some screen pixel (x right, y DOWN
// from the top-left), then we convert that pixel rectangle into NDC (x -1..1
// left..right, y -1..1 bottom..TOP). The Y axis flips, same as project().
//
// Knobs:
//   start_x / start_y : top-left of the text block, in screen pixels
//   pixel             : how many screen pixels each font dot occupies
fn build_text(
    text: &str,
    start_x: f32,
    start_y: f32,
    pixel: f32,
    screen_w: f32,
    screen_h: f32,
) -> Vec<TextInstance> {
    let mut out = Vec::new();
    let mut cursor_x = start_x;
    let mut cursor_y = start_y;

    for c in text.chars() {
        if c == '\n' {
            cursor_x = start_x;
            cursor_y += (font::GLYPH_HEIGHT as f32 + 1.0) * pixel;
            continue;
        }

        let rows = font::glyph(c);
        for (row_i, &bits) in rows.iter().enumerate() {
            for col in 0..font::GLYPH_WIDTH {
                // bit 4 is the leftmost column, bit 0 the rightmost.
                let lit = (bits >> (font::GLYPH_WIDTH - 1 - col)) & 1 == 1;
                if !lit {
                    continue;
                }

                // This font dot's top-left, in screen pixels.
                let sx = cursor_x + col as f32 * pixel;
                let sy = cursor_y + row_i as f32 * pixel;

                // Convert to an NDC rectangle. The quad's offset is its
                // bottom-left corner and it grows +x (right) and +y (up), so we
                // anchor at the dot's BOTTOM edge (sy + pixel) after the flip.
                let ndc_x = sx / screen_w * 2.0 - 1.0;
                let ndc_y_bottom = 1.0 - (sy + pixel) / screen_h * 2.0;

                out.push(TextInstance {
                    offset: [ndc_x, ndc_y_bottom],
                    size: [pixel / screen_w * 2.0, pixel / screen_h * 2.0],
                });
            }
        }

        cursor_x += (font::GLYPH_WIDTH as f32 + 1.0) * pixel;
    }

    out
}

// Build the camera's view-projection matrix from its current state.
//
// The eye orbits the focus point at `distance`, swung around by yaw (around the
// world Y axis) and pitch (elevation). yaw = pitch = 0 puts the eye straight out
// along +Z, i.e. looking down the -Z axis — the old fixed view.
fn camera_matrix(
    focus_x: f32,
    focus_y: f32,
    distance: f32,
    yaw: f32,
    pitch: f32,
    width: u32,
    height: u32,
) -> Mat4 {
    let aspect = width as f32 / height.max(1) as f32;

    let target = Vec3::new(focus_x, focus_y, 0.0);
    let offset = Vec3::new(
        pitch.cos() * yaw.sin(),
        pitch.sin(),
        pitch.cos() * yaw.cos(),
    ) * distance;
    let eye = target + offset;
    let up = Vec3::Y;

    let view = Mat4::look_at_rh(eye, target, up);
    let proj = Mat4::perspective_rh(FOV_DEGREES.to_radians(), aspect, 0.1, 10000.0);

    proj * view
}

fn camera_view_proj(
    focus_x: f32,
    focus_y: f32,
    distance: f32,
    yaw: f32,
    pitch: f32,
    width: u32,
    height: u32,
) -> [[f32; 4]; 4] {
    camera_matrix(focus_x, focus_y, distance, yaw, pitch, width, height).to_cols_array_2d()
}

// Project a world point through the view-projection matrix to screen pixels.
fn project(vp: Mat4, x: f32, y: f32, z: f32, width: f32, height: f32) -> Option<(f32, f32)> {
    let clip = vp * Vec4::new(x, y, z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    let screen_x = (ndc_x * 0.5 + 0.5) * width;
    let screen_y = (1.0 - (ndc_y * 0.5 + 0.5)) * height;
    Some((screen_x, screen_y))
}

// All the long-lived GPU objects, bundled so they travel together.
struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    render_pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    depth_view: wgpu::TextureView,
    egui_renderer: egui_wgpu::Renderer,
}

impl GpuState {
    fn new(window: Arc<Window>) -> GpuState {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone()).unwrap();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .unwrap();

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();

        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .unwrap();
        surface.configure(&device, &config);

        let depth_view = create_depth_view(&device, &config);

        // egui's renderer. It draws in its own pass with no depth attachment,
        // so RendererOptions::default() (depth_stencil_format: None) is correct.
        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            config.format,
            egui_wgpu::RendererOptions::default(),
        );

        let camera_uniform = CameraUniform {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera buffer"),
            contents: bytemuck::cast_slice(&[camera_uniform]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

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

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        // --- Entity pipeline (world-space, camera-driven) ---
        let entity_shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));

        let entity_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("entity pipeline layout"),
                bind_group_layouts: &[Some(&camera_bind_group_layout)],
                immediate_size: 0,
            });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("entity pipeline"),
            layout: Some(&entity_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &entity_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[InstanceRaw::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &entity_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            // Entities test AND write depth: nearer fragments win and record
            // their depth, so a later-drawn far fragment is correctly discarded.
            // This is what makes a solid cube (front faces hide back faces).
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- Text/overlay pipeline (screen-space, no camera) ---
        let text_shader = device.create_shader_module(wgpu::include_wgsl!("text.wgsl"));

        let text_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text pipeline layout"),
            bind_group_layouts: &[], // no camera — positions are already in NDC
            immediate_size: 0,
        });

        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text pipeline"),
            layout: Some(&text_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &text_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[TextInstance::layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &text_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            // The overlay is screen furniture: it must ALWAYS draw on top and
            // never write depth (Always = ignore the test, write disabled).
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
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
            text_pipeline,
            camera_buffer,
            camera_bind_group,
            depth_view,
            egui_renderer,
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            // Depth buffer must track the window size, or the test reads garbage.
            self.depth_view = create_depth_view(&self.device, &self.config);
        }
    }

    // Draw one frame: entities (world-space cubes) then text (screen overlay).
    fn render(
        &mut self,
        instances: &[InstanceRaw],
        text_instances: &[TextInstance],
        view_proj: [[f32; 4]; 4],
        egui_paint_jobs: &[egui::epaint::ClippedPrimitive],
        egui_textures_delta: &egui::TexturesDelta,
        egui_ppp: f32,
    ) {
        let camera_uniform = CameraUniform { view_proj };
        self.queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::cast_slice(&[camera_uniform]),
        );

        // egui: upload any new/changed textures before we start encoding.
        for (id, image_delta) in &egui_textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, image_delta);
        }
        let egui_screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: egui_ppp,
        };

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

        let text_buffer = if text_instances.is_empty() {
            None
        } else {
            Some(
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("text buffer"),
                        contents: bytemuck::cast_slice(text_instances),
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

        // egui: record its vertex/index uploads into the encoder. Must happen
        // before any render pass is active.
        let egui_user_buffers = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            egui_paint_jobs,
            &egui_screen,
        );

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
                // Clear depth to 1.0 (farthest) at the start of every frame.
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // entities — 36 vertices per cube (6 faces x 2 tris x 3 verts)
            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            if let Some(buffer) = &instance_buffer {
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..36, 0..instances.len() as u32);
            }

            // text overlay (screen-space, drawn on top, no camera)
            render_pass.set_pipeline(&self.text_pipeline);
            if let Some(buffer) = &text_buffer {
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..6, 0..text_instances.len() as u32);
            }
        }

        // egui pass: layered over the scene (load, don't clear), no depth.
        // egui's renderer requires a RenderPass<'static>, hence forget_lifetime.
        {
            let mut egui_pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui_renderer
                .render(&mut egui_pass, egui_paint_jobs, &egui_screen);
        }

        // egui: free textures it no longer needs, after this frame's draw.
        for id in &egui_textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        // egui's upload command buffers must be submitted before the main one.
        self.queue.submit(
            egui_user_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        frame.present();
    }
}

/// Which tab is showing in the right-hand inspector dock.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Scene,
    Inspector,
}

/// Which tab is showing in the bottom console dock.
#[derive(Clone, Copy, PartialEq)]
enum ConsoleTab {
    Output,
    Terminal,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    world: World,
    paused: bool,
    clock: Clock,
    cam_focus_x: f32,
    cam_focus_y: f32,
    cam_distance: f32,
    cam_yaw: f32,
    cam_pitch: f32,
    dragging: bool,
    orbiting: bool,
    last_cursor: (f64, f64),
    selected: Option<usize>,
    show_help: bool,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    // Active tab in the right-hand inspector dock.
    current_tab: Tab,
    // Active tab in the bottom console dock.
    console_tab: ConsoleTab,
    // Lines shown in the console Output tab (also echoed to the terminal).
    log_lines: Vec<String>,
    // Which movement buttons (WASD) are currently held, read by the input system.
    input: InputState,

    // GPU texture for the toolbar logo, uploaded once on the first frame.
    logo_texture: Option<egui::TextureHandle>,
}

impl App {
    /// Append a line to the in-editor log (shown in the console Output tab) and
    /// echo it to the terminal. The buffer is capped so it can't grow forever.
    fn log(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        println!("{msg}");
        self.log_lines.push(msg);
        if self.log_lines.len() > 500 {
            self.log_lines.remove(0);
        }
    }

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
            self.cam_yaw,
            self.cam_pitch,
            width_u,
            height_u,
        );

        let cursor_x = self.last_cursor.0 as f32;
        let cursor_y = self.last_cursor.1 as f32;
        let mut picked: Option<usize> = None;
        for (id, slot) in self.world.positions.iter().enumerate() {
            if let Some(p) = slot {
                // Hit-box grows with the entity's scale so picking matches what's drawn.
                let scale = self.world.scales.get(id).copied().unwrap_or_default();
                let half = QUAD_SIZE * 0.5 * scale.factor;
                let center = project(vp, p.x, p.y, p.z, width, height);
                let corner = project(vp, p.x + half, p.y + half, p.z, width, height);
                if let (Some((cx, cy)), Some((ex, ey))) = (center, corner) {
                    let half_w = (ex - cx).abs();
                    let half_h = (ey - cy).abs();
                    if (cursor_x - cx).abs() <= half_w && (cursor_y - cy).abs() <= half_h {
                        picked = Some(id);
                    }
                }
            }
        }

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

// Decode the embedded logo into a winit window icon (shown in the title bar and
// the OS taskbar). Returns None if it can't decode, so the window still opens.
// NOTE: honoured on X11 and Windows; Wayland ignores it and takes the taskbar
// icon from a matching .desktop file instead.
fn load_window_icon() -> Option<Icon> {
    let rgba = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), width, height).ok()
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[allow(unused_mut)]
        let mut attributes = Window::default_attributes()
            .with_title("Frame Editor")
            .with_window_icon(load_window_icon());
        // Wayland ignores the in-process icon above; it matches this app-id to a
        // frame-editor.desktop file and reads the taskbar icon from there.
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes = attributes.with_name("frame-editor", "frame-editor");
        }
        let window = Arc::new(event_loop.create_window(attributes).unwrap());
        self.gpu = Some(GpuState::new(window.clone()));

        // egui input state. Lives here because it needs the window; the egui
        // Context it shares already exists on App.
        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            None,
            None,
            None,
        );
        self.egui_state = Some(egui_state);

        window.request_redraw();
        self.window = Some(window);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Release GPU and window resources here, while winit's platform
        // connection (the Wayland display) is still alive. If we let these drop
        // later, after the event loop has torn down, the wgpu surface's
        // destructor touches Wayland objects that are already gone, which is the
        // segfault on exit. Order matters: GPU state first (its surface holds a
        // handle to the window), then egui, then the window last.
        self.gpu = None;
        self.egui_state = None;
        self.window = None;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Feed every event to egui so its own widgets (dragging the panel,
        // future buttons/sliders) keep working. We deliberately ignore the
        // returned `consumed` flag: in 0.35 it's driven by egui's *interaction*
        // state, which is the wrong question for a viewport editor and gates
        // press/release differently once a button goes down — that's what stuck
        // our drag/orbit state and killed viewport input.
        if let (Some(state), Some(window)) = (self.egui_state.as_mut(), self.window.as_ref()) {
            let _ = state.on_window_event(window, &event);
        }

        // Is the cursor over an egui panel? Because we now drive egui through
        // run_ui — which establishes the root available-rect — egui's own test
        // works: true over the toolbar/inspector, false over the 3D viewport.
        // Press and release are judged identically (by location), so drag/orbit
        // state can never get stuck.
        let pointer_over_ui = self.egui_ctx.is_pointer_over_egui();
        let ui_wants_keys = self.egui_ctx.egui_wants_keyboard_input();

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
            WindowEvent::MouseInput { state, button, .. } if !pointer_over_ui => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Left => {
                        self.dragging = pressed;
                        if pressed {
                            self.pick();
                        }
                    }
                    // Middle button held = orbit the camera.
                    MouseButton::Middle => {
                        self.orbiting = pressed;
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let dx = (position.x - self.last_cursor.0) as f32;
                let dy = (position.y - self.last_cursor.1) as f32;

                if self.orbiting {
                    // Sweep the orbit. Drag right -> swing around; drag up ->
                    // rise over the top. Pitch is clamped just short of the
                    // poles so the up vector never degenerates.
                    self.cam_yaw += dx * ORBIT_SENS;
                    self.cam_pitch -= dy * ORBIT_SENS;
                    self.cam_pitch = self.cam_pitch.clamp(-1.4, 1.4);
                } else if self.dragging {
                    if let Some(window) = &self.window {
                        let height_px = window.inner_size().height.max(1) as f32;
                        let visible_world_height =
                            2.0 * self.cam_distance * (FOV_DEGREES.to_radians() * 0.5).tan();
                        let world_per_px = visible_world_height / height_px;

                        self.cam_focus_x -= dx * world_per_px;
                        self.cam_focus_y += dy * world_per_px;
                    }
                }
                self.last_cursor = (position.x, position.y);
            }
            WindowEvent::MouseWheel { delta, .. } if !pointer_over_ui => {
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
                // Level-triggered movement input (WASD). Updated on both press
                // and release so the input system always sees what is held right
                // now. Tracked even when egui holds keyboard focus — otherwise
                // ticking the Controlled checkbox (which keeps focus) would
                // silently swallow WASD and the entity could never be driven.
                // The edge-triggered editor actions below still defer to egui
                // via `ui_wants_keys`.
                if let PhysicalKey::Code(code) = event.physical_key {
                    let pressed = event.state == ElementState::Pressed;
                    match code {
                        KeyCode::KeyW => self.input.set(Button::Up, pressed),
                        KeyCode::KeyA => self.input.set(Button::Left, pressed),
                        KeyCode::KeyS => self.input.set(Button::Down, pressed),
                        KeyCode::KeyD => self.input.set(Button::Right, pressed),
                        _ => {}
                    }
                }

                if !ui_wants_keys && event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        // Position nudge — moves the selected entity along a world
                        // axis. Runs on auto-repeat too, so holding a key glides.
                        let nudge = match code {
                            KeyCode::ArrowLeft => Some((-EDIT_STEP, 0.0, 0.0)),
                            KeyCode::ArrowRight => Some((EDIT_STEP, 0.0, 0.0)),
                            KeyCode::ArrowUp => Some((0.0, EDIT_STEP, 0.0)),
                            KeyCode::ArrowDown => Some((0.0, -EDIT_STEP, 0.0)),
                            KeyCode::PageUp => Some((0.0, 0.0, EDIT_STEP)),
                            KeyCode::PageDown => Some((0.0, 0.0, -EDIT_STEP)),
                            _ => None,
                        };

                        if let Some((dx, dy, dz)) = nudge {
                            if let Some(id) = self.selected {
                                if let Some(p) = self.world.positions.get_mut(id) {
                                    p.x += dx;
                                    p.y += dy;
                                    p.z += dz;
                                }
                            }
                        } else if !event.repeat {
                            // One-shot actions — ignore auto-repeat.
                            match code {
                                KeyCode::Space => {
                                    self.paused = !self.paused;
                                    self.log(if self.paused { "Paused" } else { "Playing" });
                                }
                                // Period steps the sim one tick while paused.
                                // Kept off S so it doesn't collide with WASD.
                                KeyCode::Period => {
                                    if self.paused {
                                        systems::movement(&mut self.world);
                                        self.log("Stepped one tick");
                                    }
                                }
                                KeyCode::Escape => {
                                    self.selected = None;
                                    self.log("Selection cleared");
                                }
                                // H: toggle the controls overlay.
                                KeyCode::KeyH => {
                                    self.show_help = !self.show_help;
                                }
                                // N: spawn a new entity at the camera focus, and select it.
                                KeyCode::KeyN => {
                                    let id = self.world.spawn(
                                        Position {
                                            x: self.cam_focus_x,
                                            y: self.cam_focus_y,
                                            z: 0.0,
                                        },
                                        Velocity {
                                            dx: 0.0,
                                            dy: 0.0,
                                            dz: 0.0,
                                        },
                                    );
                                    self.selected = Some(id);
                                    self.log(format!("Spawned entity {id}"));
                                }
                                // Delete: despawn the selected entity.
                                KeyCode::Delete => {
                                    if let Some(id) = self.selected {
                                        self.world.despawn(id);
                                        self.selected = None;
                                        self.log(format!("Despawned entity {id}"));
                                    }
                                }
                                // F5: save the current world to disk.
                                KeyCode::F5 => match self.world.save_to_file(SCENE_PATH) {
                                    Ok(()) => self.log(format!("Saved scene to {SCENE_PATH}")),
                                    Err(e) => self.log(format!("Save failed: {e}")),
                                },
                                // F9: reload the world from disk, discarding the current one.
                                KeyCode::F9 => match World::load_from_file(SCENE_PATH) {
                                    Ok(world) => {
                                        self.world = world;
                                        self.selected = None;
                                        self.log(format!("Reloaded scene from {SCENE_PATH}"));
                                    }
                                    Err(e) => self.log(format!("Reload failed: {e}")),
                                },
                                _ => {}
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let owed = self.clock.advance(!self.paused);
                for _ in 0..owed {
                    systems::input_movement(&mut self.world, &self.input);
                    systems::movement(&mut self.world);
                }

                let selected = self.selected;
                let instances: Vec<InstanceRaw> = self
                    .world
                    .positions
                    .iter()
                    .enumerate()
                    .filter_map(|(id, slot)| slot.as_ref().map(|p| (id, p)))
                    .map(|(id, p)| {
                        // Falls back to the default colour if this entity has no colour slot,
                        // which only happens for scenes saved before colours existed.
                        let color = self.world.colors.get(id).copied().unwrap_or_default();
                        let scale = self.world.scales.get(id).copied().unwrap_or_default();
                        InstanceRaw {
                            position: [p.x, p.y, p.z],
                            color: [color.r, color.g, color.b],
                            selected: if Some(id) == selected { 1.0 } else { 0.0 },
                            scale: scale.factor,
                        }
                    })
                    .collect();

                let (width, height) = match &self.window {
                    Some(window) => {
                        let size = window.inner_size();
                        (size.width, size.height)
                    }
                    None => (1, 1),
                };

                // The selected entity's ID/POS/VEL now live in the Inspector
                // panel, so the old top-left readout is retired. The controls
                // legend below is the only remaining hand-rolled overlay.
                let mut text_instances: Vec<TextInstance> = Vec::new();

                // Controls overlay (toggle with H), anchored bottom-left. Drawn
                // at a smaller pixel size than the inspector so it reads as
                // secondary furniture.
                if self.show_help {
                    let help = "CONTROLS   H TO HIDE\n\n\
                                                    SPACE  PLAY PAUSE\n\n\
                                                    .  STEP WHEN PAUSED\n\n\
                                                    WASD  DRIVE CONTROLLED\n\n\
                                                    N  SPAWN ENTITY\n\n\
                                                    DEL  DESPAWN SELECTED\n\n\
                                                    ARROWS  MOVE X Y\n\n\
                                                    PGUP PGDN  MOVE Z\n\n\
                                                    F5 SAVE   F9 LOAD\n\n\
                                                    ESC  DESELECT\n\n\
                                                    LMB  SELECT   DRAG PAN\n\n\
                                                    MMB  DRAG ORBIT\n\n\
                                                    WHEEL  ZOOM";
                    let pixel = 2.0;
                    let lines = help.lines().count() as f32;
                    let line_h = (font::GLYPH_HEIGHT as f32 + 1.0) * pixel;
                    let start_y = height as f32 - lines * line_h - 16.0;
                    text_instances.extend(build_text(
                        help,
                        16.0,
                        start_y,
                        pixel,
                        width as f32,
                        height as f32,
                    ));
                }

                let view_proj = camera_view_proj(
                    self.cam_focus_x,
                    self.cam_focus_y,
                    self.cam_distance,
                    self.cam_yaw,
                    self.cam_pitch,
                    width,
                    height,
                );

                // --- Run egui for this frame ---
                // run_ui hands our closure a full-screen root Ui and runs the
                // begin/end pass internally. Panels shown into that root dock to
                // the window edges. In egui 0.35 the old SidePanel/TopBottomPanel
                // types were merged into one `Panel` (Panel::top/right/bottom/...).
                // Panels are solid but resizable (drag their inner edge); the
                // tabs/content are still a mockup wired to nothing but text and
                // the live log. We move state in/out via locals so the closure
                // never has to borrow `self`.
                let mut tab = self.current_tab;
                let mut console_tab = self.console_tab;
                let log_lines = std::mem::take(&mut self.log_lines);
                // Live entity ids for the Scene list, plus the selected entity's
                // position/velocity lifted into a local so the egui closure never
                // touches `self`. Any edits get written back into the world after
                // the pass; selection changes flow through `new_selection`.
                let entity_ids: Vec<usize> = self
                    .world
                    .positions
                    .iter()
                    .enumerate()
                    .filter_map(|(i, slot)| slot.as_ref().map(|_| i))
                    .collect();
                let mut new_selection = self.selected;
                let mut edited = self.selected.and_then(|id| {
                    Some((
                        id,
                        *self.world.positions.get(id)?,
                        *self.world.velocities.get(id)?,
                        self.world.colors.get(id).copied().unwrap_or_default(),
                        self.world.controlled.get(id).is_some(),
                        self.world.scales.get(id).copied().unwrap_or_default(),
                    ))
                });

                // Upload the logo to the GPU on the first frame, then reuse the handle.
                if self.logo_texture.is_none() {
                    let rgba = image::load_from_memory(LOGO_PNG)
                        .expect("embedded logo PNG should decode")
                        .to_rgba8();
                    let (w, h) = rgba.dimensions();
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        rgba.as_raw(),
                    );
                    self.logo_texture = Some(self.egui_ctx.load_texture(
                        "frame-editor-logo",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                let logo = self.logo_texture.clone();
                let (egui_paint_jobs, egui_textures_delta, egui_ppp) =
                    if let (Some(state), Some(window)) =
                        (self.egui_state.as_mut(), self.window.as_ref())
                    {
                        let raw_input = state.take_egui_input(window);
                        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
                            // Top toolbar strip — fixed height, placeholder menu.
                            egui::Panel::top("toolbar").resizable(false).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if let Some(logo) = &logo {
                                        ui.add(
                                            egui::Image::new(logo)
                                                .fit_to_exact_size(egui::vec2(20.0, 20.0)),
                                        );
                                        ui.separator();
                                    }
                                    ui.label("File");
                                    ui.separator();
                                    ui.label("Edit");
                                    ui.separator();
                                    ui.label("View");
                                    ui.separator();
                                    ui.label("Help");
                                });
                            });
                            // Bottom console dock — Output (the live log) and a
                            // Terminal placeholder. Full width; drag its top edge
                            // to resize.
                            egui::Panel::bottom("console")
                                .resizable(true)
                                .default_size(160.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.selectable_value(
                                            &mut console_tab,
                                            ConsoleTab::Output,
                                            "Output",
                                        );
                                        ui.selectable_value(
                                            &mut console_tab,
                                            ConsoleTab::Terminal,
                                            "Terminal",
                                        );
                                    });
                                    ui.separator();
                                    match console_tab {
                                        ConsoleTab::Output => {
                                            egui::ScrollArea::vertical()
                                                .stick_to_bottom(true)
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    if log_lines.is_empty() {
                                                        ui.weak("(no output yet)");
                                                    }
                                                    for line in &log_lines {
                                                        ui.monospace(line);
                                                    }
                                                });
                                        }
                                        ConsoleTab::Terminal => {
                                            ui.weak("(terminal goes here)");
                                        }
                                    }
                                });
                            // Right inspector dock — Scene / Inspector tabs.
                            // Drag its left edge to resize.
                            egui::Panel::right("inspector")
                                .resizable(true)
                                .default_size(260.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.selectable_value(&mut tab, Tab::Scene, "Scene");
                                        ui.selectable_value(&mut tab, Tab::Inspector, "Inspector");
                                    });
                                    ui.separator();
                                    match tab {
                                        Tab::Scene => {
                                            ui.label(format!("{} entities", entity_ids.len()));
                                            ui.separator();
                                            egui::ScrollArea::vertical()
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    for &id in &entity_ids {
                                                        ui.selectable_value(
                                                            &mut new_selection,
                                                            Some(id),
                                                            format!("Entity {id}"),
                                                        );
                                                    }
                                                });
                                        }
                                        Tab::Inspector => match &mut edited {
                                            Some((id, pos, vel, color, controlled, scale)) => {
                                                ui.label(format!("Entity {id}"));
                                                ui.add_space(4.0);
                                                ui.label("Position");
                                                ui.horizontal(|ui| {
                                                    ui.add(
                                                        egui::DragValue::new(&mut pos.x)
                                                            .speed(1.0)
                                                            .prefix("x "),
                                                    );
                                                    ui.add(
                                                        egui::DragValue::new(&mut pos.y)
                                                            .speed(1.0)
                                                            .prefix("y "),
                                                    );
                                                    ui.add(
                                                        egui::DragValue::new(&mut pos.z)
                                                            .speed(1.0)
                                                            .prefix("z "),
                                                    );
                                                });
                                                ui.add_space(4.0);
                                                ui.label("Velocity");
                                                ui.horizontal(|ui| {
                                                    ui.add(
                                                        egui::DragValue::new(&mut vel.dx)
                                                            .speed(0.1)
                                                            .prefix("dx "),
                                                    );
                                                    ui.add(
                                                        egui::DragValue::new(&mut vel.dy)
                                                            .speed(0.1)
                                                            .prefix("dy "),
                                                    );
                                                    ui.add(
                                                        egui::DragValue::new(&mut vel.dz)
                                                            .speed(0.1)
                                                            .prefix("dz "),
                                                    );
                                                });
                                                ui.add_space(4.0);
                                                ui.label("Color");
                                                // The widget wants a [f32; 3]; copy in, let the user
                                                // edit, copy back into Color only when it changed.
                                                let mut rgb = [color.r, color.g, color.b];
                                                if ui.color_edit_button_rgb(&mut rgb).changed() {
                                                    color.r = rgb[0];
                                                    color.g = rgb[1];
                                                    color.b = rgb[2];
                                                }
                                                ui.add_space(4.0);
                                                ui.label("Scale");
                                                ui.add(
                                                    egui::DragValue::new(&mut scale.factor)
                                                        .speed(0.05)
                                                        .range(0.1..=100.0),
                                                );
                                                ui.add_space(4.0);
                                                ui.checkbox(controlled, "Controlled (WASD)");
                                            }
                                            None => {
                                                ui.weak("No entity selected");
                                                ui.weak("Click a cube, or pick one in Scene.");
                                            }
                                        },
                                    }
                                });
                            // The space left in the middle is the 3D viewport —
                            // we draw nothing there, so the scene shows through.
                        });
                        state.handle_platform_output(window, full_output.platform_output);
                        let ppp = full_output.pixels_per_point;
                        let jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
                        (jobs, full_output.textures_delta, ppp)
                    } else {
                        (Vec::new(), egui::TexturesDelta::default(), 1.0)
                    };
                self.current_tab = tab;
                self.console_tab = console_tab;
                self.log_lines = log_lines;
                self.selected = new_selection;
                // Push any inspector edits back into the world. The render this
                // frame already used the old values; the change shows next frame
                // (same one-frame path as the keyboard nudge).
                if let Some((id, pos, vel, color, controlled, scale)) = edited {
                    if let Some(p) = self.world.positions.get_mut(id) {
                        *p = pos;
                    }
                    if let Some(v) = self.world.velocities.get_mut(id) {
                        *v = vel;
                    }
                    self.world.colors.insert(id, color);
                    self.world.scales.insert(id, scale);
                    if controlled {
                        self.world.controlled.insert(id, Controlled);
                    } else {
                        self.world.controlled.remove(id);
                    }
                }

                if let Some(gpu) = &mut self.gpu {
                    gpu.render(
                        &instances,
                        &text_instances,
                        view_proj,
                        &egui_paint_jobs,
                        &egui_textures_delta,
                        egui_ppp,
                    );
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

// The fallback scene used when there's no scene.ron on disk yet.
fn default_world() -> World {
    let mut world = World::new();

    world.spawn(
        Position {
            x: 0.0,
            y: 0.0,
            z: 40.0,
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
            dx: -0.3,
            dy: 0.3,
            dz: 0.0,
        },
    );
    world.spawn(
        Position {
            x: 0.0,
            y: 0.0,
            z: -50.0,
        },
        Velocity {
            dx: 0.0,
            dy: 0.0,
            dz: 0.6,
        },
    );

    world
}

fn main() {
    let event_loop = EventLoop::new().unwrap();

    // Load the scene from disk if one exists; otherwise start from a default
    // scene. The default is just a fallback for a fresh checkout — once you save
    // (F5), that file is what loads next time.
    let world = match World::load_from_file(SCENE_PATH) {
        Ok(world) => {
            println!("Loaded scene from {SCENE_PATH}");
            world
        }
        Err(e) => {
            println!("No scene loaded ({e}); starting from the default scene");
            default_world()
        }
    };

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
        // Gentle default tilt so the cubes read as 3D on launch. Zero both for
        // the old straight-down-Z view.
        cam_yaw: 0.5,
        cam_pitch: 0.3,
        dragging: false,
        orbiting: false,
        last_cursor: (0.0, 0.0),
        selected: None,
        show_help: true,
        egui_ctx: egui::Context::default(),
        egui_state: None,
        current_tab: Tab::Scene,
        console_tab: ConsoleTab::Output,
        log_lines: vec!["Frame Editor started.".to_string()],
        input: InputState::new(),

        logo_texture: None,
    };

    println!(
        "Editor Started, Engine World created with {} entities",
        entity_count
    );

    event_loop.run_app(&mut app).unwrap();
}
