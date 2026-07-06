const LOGO_PNG: &[u8] = include_bytes!("../assets/frame-editor.png");
use frame_engine::core::Clock;
use frame_engine::input::{Button, InputState};
use frame_engine::systems;
use frame_engine::world::{Controlled, Mesh, Position, Script, Velocity, World};
use glam::{Mat4, Vec3, Vec4};
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Icon, Window, WindowId};
mod font;
mod script;
const TICK_RATE: u32 = 30;
const MAX_CATCHUP_TICKS: u32 = 5;
// Vertical field of view, shared by the projection and the pan maths.
const FOV_DEGREES: f32 = 45.0;
// World-space size of an entity, used for picking. Defined once in the engine
// (it's a simulation fact — collision boxes use it too); we reference it here so
// pick and collision can't drift. MESH_SIZE in shader.wgsl must match it by hand.
const QUAD_SIZE: f32 = frame_engine::world::ENTITY_SIZE;
// How fast middle-drag sweeps the orbit, in radians per pixel.
const ORBIT_SENS: f32 = 0.005;
// Format of the depth buffer. 32-bit float depth, no stencil.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
// How far a single nudge moves the selected entity, in world units.
const EDIT_STEP: f32 = 5.0;
// The camera data handed to the shader. Must match the `Camera` struct in shader.wgsl.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}
// Per-vertex mesh geometry: a position in the primitive's local (roughly unit)
// space and its surface normal. One shared vertex buffer holds every primitive's
// vertices back to back; each entity instance picks which slice to draw.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshVertex {
    position: [f32; 3],
    normal: [f32; 3],
}
impl MeshVertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] = [
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
    ];
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

// --- Primitive geometry, generated once on the CPU ---
// Each primitive is built at roughly unit size (half-extent 0.5 / radius 0.5),
// centred on the origin, so the shader scales them all by the same MESH_SIZE and
// a default entity comes out exactly the size the cube always was. These are
// ordered to match the engine's `Mesh` enum: Cube, Sphere, Plane.

// A unit cube: 36 vertices (6 faces x 2 triangles), each face flat-shaded with
// one outward normal. This reproduces the cube the shader used to synthesise, so
// existing scenes look identical.
fn cube_vertices() -> Vec<MeshVertex> {
    // 8 corners of a cube with half-extent 0.5.
    const C: [[f32; 3]; 8] = [
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    // Each face: six corner indices (two triangles) and one outward normal.
    const FACES: [([usize; 6], [f32; 3]); 6] = [
        ([4, 5, 6, 6, 7, 4], [0.0, 0.0, 1.0]),  // +Z front
        ([1, 0, 3, 3, 2, 1], [0.0, 0.0, -1.0]), // -Z back
        ([5, 1, 2, 2, 6, 5], [1.0, 0.0, 0.0]),  // +X right
        ([0, 4, 7, 7, 3, 0], [-1.0, 0.0, 0.0]), // -X left
        ([3, 2, 6, 6, 7, 3], [0.0, 1.0, 0.0]),  // +Y top
        ([0, 1, 5, 5, 4, 0], [0.0, -1.0, 0.0]), // -Y bottom
    ];
    let mut verts = Vec::with_capacity(36);
    for (indices, normal) in FACES {
        for i in indices {
            verts.push(MeshVertex {
                position: C[i],
                normal,
            });
        }
    }
    verts
}

// A UV sphere of radius 0.5, built as rings of quads. The surface normal at any
// point on a sphere centred on the origin is just its (unit) direction, so the
// normal is the unit position and the position is that scaled by the radius. The
// poles produce a few degenerate (zero-area) triangles, which draw nothing.
fn sphere_vertices() -> Vec<MeshVertex> {
    use std::f32::consts::PI;
    const LAT: u32 = 12; // rings from pole to pole
    const LON: u32 = 18; // segments around
    const RADIUS: f32 = 0.5;
    let point = |theta: f32, phi: f32| -> [f32; 3] {
        [
            theta.sin() * phi.cos(),
            theta.cos(),
            theta.sin() * phi.sin(),
        ]
    };
    let vert = |unit: [f32; 3]| MeshVertex {
        position: [unit[0] * RADIUS, unit[1] * RADIUS, unit[2] * RADIUS],
        normal: unit,
    };
    let mut verts = Vec::new();
    for lat in 0..LAT {
        let t0 = PI * lat as f32 / LAT as f32;
        let t1 = PI * (lat + 1) as f32 / LAT as f32;
        for lon in 0..LON {
            let p0 = 2.0 * PI * lon as f32 / LON as f32;
            let p1 = 2.0 * PI * (lon + 1) as f32 / LON as f32;
            let a = point(t0, p0);
            let b = point(t1, p0);
            let c = point(t1, p1);
            let d = point(t0, p1);
            // two triangles per quad: (a, b, c) and (a, c, d)
            verts.push(vert(a));
            verts.push(vert(b));
            verts.push(vert(c));
            verts.push(vert(a));
            verts.push(vert(c));
            verts.push(vert(d));
        }
    }
    verts
}

// A flat, horizontal quad in the XZ plane (a floor tile), facing up (+Y),
// half-extent 0.5 — the same footprint as the cube's base. Back-face culling is
// off, so it's visible from below too.
fn plane_vertices() -> Vec<MeshVertex> {
    let normal = [0.0, 1.0, 0.0];
    let v = |x: f32, z: f32| MeshVertex {
        position: [x, 0.0, z],
        normal,
    };
    vec![
        v(-0.5, -0.5),
        v(0.5, -0.5),
        v(0.5, 0.5),
        v(0.5, 0.5),
        v(-0.5, 0.5),
        v(-0.5, -0.5),
    ]
}

// Per-entity instance data: world position plus a selected flag (0 or 1).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceRaw {
    position: [f32; 3],
    color: [f32; 3],
    selected: f32,
    scale: [f32; 3],
}
impl InstanceRaw {
    // Locations 0-1 belong to the mesh vertex buffer (MeshVertex); the instance
    // attributes continue from 2.
    const ATTRIBS: [wgpu::VertexAttribute; 4] = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 2,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress, // 12
            shader_location: 3,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32,
            offset: std::mem::size_of::<[f32; 6]>() as wgpu::BufferAddress, // 24
            shader_location: 4,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: std::mem::size_of::<[f32; 7]>() as wgpu::BufferAddress, // 28
            shader_location: 5,
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
    // One vertex buffer holding every primitive's geometry back to back, plus
    // each primitive's vertex range within it, ordered Cube, Sphere, Plane
    // (matching the engine's `Mesh` enum). Built once; static for the app's life.
    mesh_vertex_buffer: wgpu::Buffer,
    mesh_ranges: [std::ops::Range<u32>; 3],
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
        // --- Primitive geometry: one shared vertex buffer, built once ---
        // Concatenate every primitive's vertices and remember each one's range,
        // in the engine's Mesh order (Cube, Sphere, Plane). At draw time we bind
        // this buffer and draw the range for whichever primitive an entity uses.
        let cube = cube_vertices();
        let sphere = sphere_vertices();
        let plane = plane_vertices();
        let mut mesh_verts: Vec<MeshVertex> = Vec::new();
        let cube_range = 0u32..cube.len() as u32;
        mesh_verts.extend(cube);
        let sphere_range = mesh_verts.len() as u32..(mesh_verts.len() + sphere.len()) as u32;
        mesh_verts.extend(sphere);
        let plane_range = mesh_verts.len() as u32..(mesh_verts.len() + plane.len()) as u32;
        mesh_verts.extend(plane);
        let mesh_ranges = [cube_range, sphere_range, plane_range];
        let mesh_vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh vertex buffer"),
            contents: bytemuck::cast_slice(&mesh_verts),
            usage: wgpu::BufferUsages::VERTEX,
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
                buffers: &[MeshVertex::layout(), InstanceRaw::layout()],
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
            mesh_vertex_buffer,
            mesh_ranges,
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
        // How many instances belong to each primitive, in Cube, Sphere, Plane
        // order. `instances` is laid out in that same order, so these counts also
        // give each primitive's contiguous slice of the instance buffer.
        group_counts: [u32; 3],
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
            // entities — draw each primitive's instances against its own
            // geometry. The shared mesh buffer sits at slot 0; the per-entity
            // instance buffer (grouped by primitive) at slot 1. For each
            // primitive with any instances, draw its vertex range for its
            // contiguous slice of instances.
            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            if let Some(buffer) = &instance_buffer {
                render_pass.set_vertex_buffer(0, self.mesh_vertex_buffer.slice(..));
                let stride = std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress;
                let mut instance_start = 0u32;
                for (primitive, count) in group_counts.iter().enumerate() {
                    if *count > 0 {
                        // Bind just this group's slice of the instance buffer and
                        // draw from instance 0, rather than using a non-zero first
                        // instance (which some backends validate against).
                        let begin = instance_start as wgpu::BufferAddress * stride;
                        render_pass.set_vertex_buffer(1, buffer.slice(begin..));
                        render_pass.draw(self.mesh_ranges[primitive].clone(), 0..*count);
                    }
                    instance_start += count;
                }
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
/// A dockable tool panel, shown as a tab in the right-hand egui_dock area. These
/// can be dragged, tabbed together, and split apart at runtime. The 3D viewport
/// is deliberately NOT one of these — it stays the fixed background the docks
/// leave uncovered, so its input routing and transparency are unchanged.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Viewport,
    Scene,
    Inspector,
    Scripts,
}
/// Which tab is showing in the bottom console dock.
#[derive(Clone, Copy, PartialEq)]
enum ConsoleTab {
    Output,
    Terminal,
}
/// A command chosen from the toolbar menus this frame, applied after the egui
/// pass. The menu closure can't borrow `self`, so it stages the choice here and
/// we dispatch it afterwards — the same lift-then-write-back pattern the
/// selection and inspector edits use.
/// Which screen the editor is showing. It starts at the launcher; creating or
/// opening a project loads its scene and switches to the editor proper.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Launcher,
    Editor,
}

/// A choice made on the launcher screen this frame, applied after the egui pass
/// (so the blocking file dialog doesn't run mid-draw) — the same lift-then-act
/// pattern the toolbar menus use.
enum LauncherAction {
    NewProject,
    OpenProject,
    OpenRecent(std::path::PathBuf),
    PlayRecent(std::path::PathBuf),
    OpenSettings(std::path::PathBuf),
}

enum MenuAction {
    OpenScene,
    SaveScene,
    SaveSceneAs,
    ReloadScene,
    CloseProject,
    SpawnEntity,
    DespawnSelected,
    ClearSelection,
    TogglePause,
    StepOnce,
    ToggleHelp,
    About,
    Quit,
}
/// The selected entity's editable state, lifted out of the world for the
/// Inspector to edit and written back after the egui pass.
type EditedEntity = (
    usize,
    Position,
    Velocity,
    frame_engine::world::Color,
    bool,
    frame_engine::world::Scale,
    Option<String>,
    Mesh,
);

/// Scene tab: the entity list.
fn scene_tab_ui(ui: &mut egui::Ui, entity_ids: &[usize], selection: &mut Option<usize>) {
    ui.label(format!("{} entities", entity_ids.len()));
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for &id in entity_ids {
                ui.selectable_value(selection, Some(id), format!("Entity {id}"));
            }
        });
}

/// Inspector tab: the selected entity's properties.
fn inspector_tab_ui(
    ui: &mut egui::Ui,
    edited: &mut Option<EditedEntity>,
    script_library: &std::collections::BTreeMap<String, String>,
    script_filter: &mut String,
) {
    match edited {
        Some((id, pos, vel, color, controlled, scale, script_source, mesh)) => {
            ui.label(format!("Entity {id}"));
            ui.add_space(4.0);
            ui.label("Position");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut pos.x).speed(1.0).prefix("x "));
                ui.add(egui::DragValue::new(&mut pos.y).speed(1.0).prefix("y "));
                ui.add(egui::DragValue::new(&mut pos.z).speed(1.0).prefix("z "));
            });
            ui.add_space(4.0);
            ui.label("Velocity");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut vel.dx).speed(0.1).prefix("dx "));
                ui.add(egui::DragValue::new(&mut vel.dy).speed(0.1).prefix("dy "));
                ui.add(egui::DragValue::new(&mut vel.dz).speed(0.1).prefix("dz "));
            });
            ui.add_space(4.0);
            ui.label("Color");
            let mut rgb = [color.r, color.g, color.b];
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                color.r = rgb[0];
                color.g = rgb[1];
                color.b = rgb[2];
            }
            ui.add_space(4.0);
            ui.label("Scale");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut scale.x)
                        .speed(0.05)
                        .range(0.1..=100.0)
                        .prefix("x "),
                );
                ui.add(
                    egui::DragValue::new(&mut scale.y)
                        .speed(0.05)
                        .range(0.1..=100.0)
                        .prefix("y "),
                );
                ui.add(
                    egui::DragValue::new(&mut scale.z)
                        .speed(0.05)
                        .range(0.1..=100.0)
                        .prefix("z "),
                );
            });
            ui.add_space(4.0);
            ui.label("Mesh");
            let mesh_label = match *mesh {
                Mesh::Cube => "Cube",
                Mesh::Sphere => "Sphere",
                Mesh::Plane => "Plane",
            };
            egui::ComboBox::from_id_salt("mesh_picker")
                .selected_text(mesh_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(mesh, Mesh::Cube, "Cube");
                    ui.selectable_value(mesh, Mesh::Sphere, "Sphere");
                    ui.selectable_value(mesh, Mesh::Plane, "Plane");
                });
            ui.add_space(4.0);
            ui.checkbox(controlled, "Controlled (WASD)");
            ui.add_space(8.0);
            ui.label("Script");
            if script_library.is_empty() {
                ui.weak("No scripts yet — add some in the Script Editor tab.");
            } else {
                let selected_text = script_source
                    .clone()
                    .unwrap_or_else(|| "(none)".to_string());
                egui::ComboBox::from_id_salt("script_picker")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        ui.add(egui::TextEdit::singleline(script_filter).hint_text("filter…"));
                        ui.separator();
                        ui.selectable_value(script_source, None, "(none)");
                        let needle = script_filter.to_lowercase();
                        for name in script_library.keys() {
                            if needle.is_empty() || name.to_lowercase().contains(&needle) {
                                ui.selectable_value(
                                    script_source,
                                    Some(name.clone()),
                                    name.as_str(),
                                );
                            }
                        }
                    });
                if let Some(name) = script_source.as_ref() {
                    if !script_library.contains_key(name) {
                        ui.weak(format!("(uses missing script '{name}')"));
                    }
                }
            }
        }
        None => {
            ui.weak("No entity selected");
            ui.weak("Click a cube, or pick one in Scene.");
        }
    }
}

/// Script Editor tab: a script name list above one code editor. Laid out
/// vertically (rather than a left sidebar) so it reads well in a docked column.
fn scripts_tab_ui(
    ui: &mut egui::Ui,
    script_library: &mut std::collections::BTreeMap<String, String>,
    new_script_name: &mut String,
    open_script: &mut Option<String>,
    script_status: &Option<Result<(), script::ScriptError>>,
) {
    let mut delete: Option<String> = None;
    // LEFT: the script list and the "new script" box, in a resizable sidebar.
    egui::Panel::left("script_list")
        .resizable(true)
        .default_size(200.0)
        .show(ui, |ui| {
            ui.label("Scripts");
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(new_script_name)
                    .hint_text("new script name")
                    .desired_width(f32::INFINITY),
            );
            if ui.button("Add").clicked() {
                let key = new_script_name.trim().to_string();
                if !key.is_empty() {
                    script_library.entry(key.clone()).or_default();
                    *open_script = Some(key);
                    new_script_name.clear();
                }
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("script_name_list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if script_library.is_empty() {
                        ui.weak("No scripts yet.");
                    }
                    for name in script_library.keys() {
                        ui.selectable_value(open_script, Some(name.clone()), name.as_str());
                    }
                });
        });
    // RIGHT: the editor for the open script fills the space the sidebar leaves.
    let open = open_script
        .as_ref()
        .filter(|n| script_library.contains_key(*n))
        .cloned();
    match open {
        Some(name) => {
            ui.horizontal(|ui| {
                ui.strong(&name);
                if ui.small_button("Delete").clicked() {
                    delete = Some(name.clone());
                }
            });
            match script_status {
                Some(Ok(())) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x7c, 0xc5, 0x7c),
                        "No syntax errors",
                    );
                }
                Some(Err(e)) => {
                    let loc = match (e.line, e.column) {
                        (Some(l), Some(c)) => format!("line {l}, col {c}: "),
                        (Some(l), None) => format!("line {l}: "),
                        _ => String::new(),
                    };
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe0, 0x6c, 0x6c),
                        format!("{loc}{}", e.message),
                    );
                }
                None => {}
            }
            if let Some(source) = script_library.get_mut(&name) {
                let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
                let rows = ((ui.available_height() / row_h).floor() - 1.0).max(3.0) as usize;
                let line_count = source.matches('\n').count() + 1;
                let digits = line_count.to_string().len();
                let gutter: String = (1..=line_count)
                    .map(|n| format!("{n:>digits$}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                egui::ScrollArea::vertical()
                    .id_salt("script_editor")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.add_space(2.0);
                                ui.add(egui::Label::new(
                                    egui::RichText::new(&gutter).monospace().weak(),
                                ));
                            });
                            ui.add(
                                egui::TextEdit::multiline(source)
                                    .code_editor()
                                    .desired_rows(rows)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    });
            }
        }
        None => {
            ui.weak("Select a script on the left, or add one to begin.");
        }
    }
    if let Some(name) = delete {
        script_library.remove(&name);
        if open_script.as_ref() == Some(&name) {
            *open_script = None;
        }
    }
}

/// Owns the transient per-frame state the dockable tabs read and write, so
/// egui_dock's `TabViewer::ui` can reach it. Built fresh from the world each
/// frame and drained back into the world afterwards.
struct EditorTabViewer {
    entity_ids: Vec<usize>,
    selection: Option<usize>,
    edited: Option<EditedEntity>,
    script_library: std::collections::BTreeMap<String, String>,
    new_script_name: String,
    script_filter: String,
    open_script: Option<String>,
    script_status: Option<Result<(), script::ScriptError>>,
    // Set by the Viewport tab each frame to its transparent body rect (egui
    // points). `None` when the Viewport tab isn't visible. Used to route 3D
    // input: clicks/drags land on the viewport only when the cursor is here.
    viewport_rect: Option<egui::Rect>,
}

impl egui_dock::TabViewer for EditorTabViewer {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        match tab {
            Tab::Viewport => "Viewport",
            Tab::Scene => "Scene",
            Tab::Inspector => "Inspector",
            Tab::Scripts => "Script Editor",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            // The viewport draws nothing itself — the 3D scene is rendered behind
            // egui and shows through this tab's transparent body. We only record
            // the body rect so window_event can route 3D input to it.
            Tab::Viewport => {
                self.viewport_rect = Some(ui.max_rect());
            }
            Tab::Scene => scene_tab_ui(ui, &self.entity_ids, &mut self.selection),
            Tab::Inspector => inspector_tab_ui(
                ui,
                &mut self.edited,
                &self.script_library,
                &mut self.script_filter,
            ),
            Tab::Scripts => scripts_tab_ui(
                ui,
                &mut self.script_library,
                &mut self.new_script_name,
                &mut self.open_script,
                &self.script_status,
            ),
        }
    }

    /// These tool panels are always present; don't offer a close button.
    fn is_closeable(&self, _tab: &Self::Tab) -> bool {
        false
    }

    /// Leave the Viewport tab's body unpainted so the 3D scene behind egui shows
    /// through it. Every other tab clears its background normally.
    fn clear_background(&self, tab: &Self::Tab) -> bool {
        !matches!(tab, Tab::Viewport)
    }

    /// No scroll bars over the viewport — it's a fixed window onto the 3D scene.
    fn scroll_bars(&self, tab: &Self::Tab) -> [bool; 2] {
        match tab {
            Tab::Viewport => [false, false],
            _ => [true, true],
        }
    }
}

struct App {
    script_runtime: script::RhaiRuntime,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    // Launcher screen vs the editor proper.
    mode: AppMode,
    // Name of the open project (its folder name), shown in the window title.
    project_name: Option<String>,
    // Remembered projects, most-recently-edited first, shown on the launcher.
    // Refreshed when the launcher is (re)entered and on open/create.
    recent_projects: Vec<RecentProject>,
    // The name typed on the launcher for a new project (becomes its scene file).
    new_project_name: String,
    world: World,
    // Where "Save scene" writes and "Reload scene" reads. Set by Open/Save-As
    // (and defaulted to the startup scene). None means Save prompts for a path.
    current_scene_path: Option<std::path::PathBuf>,
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
    // The layout of the dockable panels (Viewport, Scene, Inspector, Scripts).
    // egui_dock owns the arrangement and which tab is active; we just persist it.
    dock_state: egui_dock::DockState<Tab>,
    // The Viewport tab's on-screen rect (egui points) from last frame, or None if
    // it wasn't visible. window_event routes 3D input by this instead of by
    // is_pointer_over_egui, which would read true over the viewport tab.
    viewport_rect: Option<egui::Rect>,
    // Active tab in the bottom console dock.
    console_tab: ConsoleTab,
    // Lines shown in the console Output tab (also echoed to the terminal).
    log_lines: Vec<String>,
    // Which movement buttons (WASD) are currently held, read by the input system.
    input: InputState,
    // Text in the "new script name" box on the Scripts tab (persists between frames).
    new_script_name: String,
    // Filter text for the Inspector's script picker (persists between frames).
    script_filter: String,
    // Which library script is open in the Script Editor's centre pane (by name).
    open_script: Option<String>,
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
    // --- Editor actions ---
    // One definition per action. The keyboard and the menus are just two
    // triggers that call these; the behaviour lives in exactly one place.
    /// Toggle the simulation between playing and paused.
    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.log(if self.paused { "Paused" } else { "Playing" });
    }
    /// Advance the simulation exactly one tick. Only meaningful while paused.
    fn step_once(&mut self) {
        if self.paused {
            systems::movement(&mut self.world);
            self.log("Stepped one tick");
        }
    }
    /// Clear the current selection.
    fn clear_selection(&mut self) {
        self.selected = None;
        self.log("Selection cleared");
    }
    /// Toggle the on-screen controls overlay.
    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }
    /// Spawn a new entity at the camera focus and select it.
    fn spawn_at_focus(&mut self) {
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
    /// Despawn the selected entity, if any.
    fn despawn_selected(&mut self) {
        if let Some(id) = self.selected {
            self.world.despawn(id);
            self.selected = None;
            self.log(format!("Despawned entity {id}"));
        }
    }
    /// Save the current world to disk.
    /// Save to the current scene path, or fall back to "Save as…" if there
    /// isn't one yet.
    fn save_scene(&mut self) {
        match self.current_scene_path.clone() {
            Some(path) => match self.world.save_to_file(&path) {
                Ok(()) => self.log(format!("Saved scene to {}", path.display())),
                Err(e) => self.log(format!("Save failed: {e}")),
            },
            None => self.save_scene_as(),
        }
    }

    /// Ask for a path with a native file dialog, save there, and remember it as
    /// the current scene.
    fn save_scene_as(&mut self) {
        let picked = rfd::FileDialog::new()
            .add_filter("Frame scene", &["ron"])
            .set_file_name("scene.ron")
            .set_title("Save scene as")
            .save_file();
        if let Some(path) = picked {
            match self.world.save_to_file(&path) {
                Ok(()) => {
                    self.log(format!("Saved scene to {}", path.display()));
                    self.current_scene_path = Some(path);
                }
                Err(e) => self.log(format!("Save failed: {e}")),
            }
        }
    }

    /// Ask for a scene file with a native file dialog and load it, replacing the
    /// current world and making it the current scene.
    fn open_scene(&mut self) {
        let picked = rfd::FileDialog::new()
            .add_filter("Frame scene", &["ron"])
            .set_title("Open scene")
            .pick_file();
        if let Some(path) = picked {
            match World::load_from_file(&path) {
                Ok(world) => {
                    self.world = world;
                    self.selected = None;
                    self.log(format!("Opened scene from {}", path.display()));
                    self.current_scene_path = Some(path);
                }
                Err(e) => self.log(format!("Open failed: {e}")),
            }
        }
    }

    /// Reload the world from the current scene path, discarding the current one.
    fn reload_scene(&mut self) {
        let Some(path) = self.current_scene_path.clone() else {
            self.log("No scene to reload — open or save one first".to_string());
            return;
        };
        match World::load_from_file(&path) {
            Ok(world) => {
                self.world = world;
                self.selected = None;
                self.log(format!("Reloaded scene from {}", path.display()));
            }
            Err(e) => self.log(format!("Reload failed: {e}")),
        }
    }

    /// Draw the launcher screen: an egui-only frame (no simulation, no 3D) with
    /// buttons to create or open a project. Mirrors the editor's egui→render
    /// handoff, but renders an empty 3D scene behind the UI.
    fn draw_launcher(&mut self) {
        let mut action: Option<LauncherAction> = None;
        let recent = self.recent_projects.clone();
        let mut name_input = std::mem::take(&mut self.new_project_name);
        let (jobs, tex_delta, ppp) = if let (Some(state), Some(window)) =
            (self.egui_state.as_mut(), self.window.as_ref())
        {
            let raw_input = state.take_egui_input(window);
            let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.add_space(12.0);
                    ui.heading("Frame Editor");
                    ui.add_space(10.0);
                    // Header bar: create a named project, or open an existing one.
                    ui.horizontal(|ui| {
                        ui.label("New project:");
                        ui.add(
                            egui::TextEdit::singleline(&mut name_input)
                                .hint_text("name")
                                .desired_width(200.0),
                        );
                        if ui.button("Create…").clicked() {
                            action = Some(LauncherAction::NewProject);
                        }
                        ui.add_space(16.0);
                        if ui.button("Open existing project…").clicked() {
                            action = Some(LauncherAction::OpenProject);
                        }
                    });
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);

                    if recent.is_empty() {
                        ui.weak("No projects yet — create one above to begin.");
                    } else {
                        ui.heading("Projects");
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for proj in &recent {
                                    ui.group(|ui| {
                                        // Fill the width so each project is a
                                        // full-width row: name and date on the
                                        // left, actions pushed to the right.
                                        ui.set_min_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    egui::RichText::new(&proj.name)
                                                        .size(18.0)
                                                        .strong(),
                                                );
                                                ui.add_space(2.0);
                                                ui.weak(format!(
                                                    "Last edited {}",
                                                    format_edited(proj.modified)
                                                ));
                                            });
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if ui.button("Settings").clicked() {
                                                        action =
                                                            Some(LauncherAction::OpenSettings(
                                                                proj.root.clone(),
                                                            ));
                                                    }
                                                    if ui.button("Play").clicked() {
                                                        action = Some(LauncherAction::PlayRecent(
                                                            proj.root.clone(),
                                                        ));
                                                    }
                                                    if ui.button("Edit").clicked() {
                                                        action = Some(LauncherAction::OpenRecent(
                                                            proj.root.clone(),
                                                        ));
                                                    }
                                                },
                                            );
                                        });
                                    });
                                    ui.add_space(6.0);
                                }
                            });
                    }
                });
            });
            state.handle_platform_output(window, full_output.platform_output);
            let ppp = full_output.pixels_per_point;
            let jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
            (jobs, full_output.textures_delta, ppp)
        } else {
            (Vec::new(), egui::TexturesDelta::default(), 1.0)
        };
        if let Some(gpu) = &mut self.gpu {
            gpu.render(
                &[],
                [0, 0, 0],
                &[],
                Mat4::IDENTITY.to_cols_array_2d(),
                &jobs,
                &tex_delta,
                ppp,
            );
        }
        self.new_project_name = name_input;
        match action {
            Some(LauncherAction::NewProject) => self.new_project(),
            Some(LauncherAction::OpenProject) => self.open_project(),
            Some(LauncherAction::OpenRecent(root)) => self.open_project_at(root),
            Some(LauncherAction::PlayRecent(_)) => {
                self.log("Play mode (a separate game window) is coming next.".to_string());
            }
            Some(LauncherAction::OpenSettings(_)) => {
                self.log(
                    "Project settings (name, description, version) are coming next.".to_string(),
                );
            }
            None => {}
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Create a project: take the name typed on the launcher, pick a folder,
    /// and scaffold `<name>.ron` (a starter scene) into it. The project's name is
    /// that scene file's stem.
    fn new_project(&mut self) {
        let name = self.new_project_name.trim().to_string();
        if name.is_empty() {
            self.log("Type a project name first".to_string());
            return;
        }
        if name.contains(['/', '\\']) {
            self.log("Project name can't contain slashes".to_string());
            return;
        }
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose a folder for the new project")
            .pick_folder()
        else {
            return;
        };
        let scene_path = folder.join(format!("{name}.ron"));
        let world = default_world();
        match world.save_to_file(&scene_path) {
            Ok(()) => {
                self.world = world;
                self.new_project_name.clear();
                self.add_recent_project(&folder);
                self.enter_editor(name, scene_path);
            }
            Err(e) => self.log(format!("Could not create project: {e}")),
        }
    }

    /// Open a project: pick its folder and load the scene inside it.
    fn open_project(&mut self) {
        let Some(root) = rfd::FileDialog::new()
            .set_title("Open a project folder")
            .pick_folder()
        else {
            return;
        };
        self.open_project_at(root);
    }

    /// Open the project at a known folder (the folder picker and the recent
    /// list both route here). Loads the folder's scene file.
    fn open_project_at(&mut self, root: std::path::PathBuf) {
        let Some(scene_path) = find_scene(&root) else {
            self.log("That folder has no scene to open".to_string());
            return;
        };
        let name = scene_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Project")
            .to_string();
        match World::load_from_file(&scene_path) {
            Ok(world) => {
                self.world = world;
                self.add_recent_project(&root);
                self.enter_editor(name, scene_path);
            }
            Err(e) => self.log(format!("Could not open project: {e}")),
        }
    }

    /// Record a project as recently used and refresh the sorted launcher list.
    fn add_recent_project(&mut self, root: &std::path::Path) {
        let mut roots = read_recent_projects();
        roots.retain(|r| r != root);
        roots.insert(0, root.to_path_buf());
        roots.truncate(20);
        write_recent_projects(&roots);
        self.recent_projects = sorted_recent_projects();
    }

    /// Close the open project and return to the launcher. Does not save — use
    /// Save (F5) first to keep changes.
    fn close_project(&mut self) {
        self.mode = AppMode::Launcher;
        self.project_name = None;
        self.current_scene_path = None;
        self.selected = None;
        self.paused = false;
        self.world = World::default();
        self.recent_projects = sorted_recent_projects();
        if let Some(window) = &self.window {
            window.set_title("Frame Editor");
        }
    }

    /// Switch from the launcher into the editor with a project loaded.
    fn enter_editor(&mut self, name: String, scene_path: std::path::PathBuf) {
        self.current_scene_path = Some(scene_path);
        self.selected = None;
        self.paused = false;
        self.mode = AppMode::Editor;
        if let Some(window) = &self.window {
            window.set_title(&format!("Frame Editor — {name}"));
        }
        self.log(format!("Opened project '{name}'"));
        self.project_name = Some(name);
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
                let half_x = QUAD_SIZE * 0.5 * scale.x;
                let half_y = QUAD_SIZE * 0.5 * scale.y;
                let center = project(vp, p.x, p.y, p.z, width, height);
                let corner = project(vp, p.x + half_x, p.y + half_y, p.z, width, height);
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
        // The viewport is now an egui_dock tab, so is_pointer_over_egui() reads
        // true over it and would kill 3D input. Instead we route 3D input by the
        // viewport tab's own body rect: allow picking/orbit/zoom only when the
        // cursor is inside it. viewport_rect is in egui points; the winit cursor
        // is in physical pixels, so divide by pixels_per_point to compare. None
        // (viewport tab hidden behind another tab) means no 3D input.
        let over_viewport = self.viewport_rect.is_some_and(|r| {
            let ppp = self.egui_ctx.pixels_per_point();
            let p = egui::pos2(
                self.last_cursor.0 as f32 / ppp,
                self.last_cursor.1 as f32 / ppp,
            );
            r.contains(p)
        });
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
            WindowEvent::MouseInput { state, button, .. } if over_viewport => {
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
            WindowEvent::MouseWheel { delta, .. } if over_viewport => {
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
            WindowEvent::KeyboardInput { event, .. } if matches!(self.mode, AppMode::Editor) => {
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
                            // One-shot actions — fire once per fresh press (no
                            // auto-repeat). Each calls the same method the menus do.
                            match code {
                                KeyCode::Space => self.toggle_pause(),
                                // Period steps the sim one tick while paused.
                                // Kept off S so it doesn't collide with WASD.
                                KeyCode::Period => self.step_once(),
                                KeyCode::Escape => self.clear_selection(),
                                // H: toggle the controls overlay.
                                KeyCode::KeyH => self.toggle_help(),
                                // N: spawn a new entity at the camera focus, and select it.
                                KeyCode::KeyN => self.spawn_at_focus(),
                                // Delete: despawn the selected entity.
                                KeyCode::Delete => self.despawn_selected(),
                                // F5: save the current world to disk.
                                KeyCode::F5 => self.save_scene(),
                                // F9: reload the world from disk, discarding the current one.
                                KeyCode::F9 => self.reload_scene(),
                                _ => {}
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // On the launcher screen there's no simulation and no 3D — draw
                // just the launcher UI and stop.
                if matches!(self.mode, AppMode::Launcher) {
                    self.draw_launcher();
                    return;
                }
                let owed = self.clock.advance(!self.paused);
                for _ in 0..owed {
                    // Detection runs first each tick, so a script can read whether
                    // its entity is colliding *this* tick (via the `hit` variable)
                    // and react before movement is applied.
                    systems::collision(&mut self.world);
                    systems::run_scripts(&mut self.world, &mut self.script_runtime);
                    systems::input_movement(&mut self.world, &self.input);
                    systems::movement(&mut self.world);
                }
                // Refresh collisions once more for the editor's red tint. Inside
                // the loop, detection ran at each tick's start (before that tick's
                // movement); this recomputes it at the final, on-screen positions,
                // and also keeps the tint live while paused (when the loop above
                // doesn't run at all) or while dragging entities around.
                systems::collision(&mut self.world);
                let colliding: std::collections::HashSet<usize> = self
                    .world
                    .collisions
                    .iter()
                    .flat_map(|&(a, b)| [a, b])
                    .collect();
                let selected = self.selected;
                // Group instances by primitive so the renderer can draw each
                // shape in one call. Buckets are kept in the engine's Mesh order
                // (Cube, Sphere, Plane); an entity with no mesh slot (an old
                // scene) falls back to Cube via unwrap_or_default.
                let mut cube_i: Vec<InstanceRaw> = Vec::new();
                let mut sphere_i: Vec<InstanceRaw> = Vec::new();
                let mut plane_i: Vec<InstanceRaw> = Vec::new();
                for (id, slot) in self.world.positions.iter().enumerate() {
                    let Some(p) = slot.as_ref() else { continue };
                    let color = self.world.colors.get(id).copied().unwrap_or_default();
                    let scale = self.world.scales.get(id).copied().unwrap_or_default();
                    let mesh = self.world.meshes.get(id).copied().unwrap_or_default();
                    // Tint entities that are currently overlapping toward red so a
                    // collision is obvious at a glance. Purely visual — detection
                    // changes nothing about the entity's own colour in the world.
                    let rgb = if colliding.contains(&id) {
                        const T: f32 = 0.6; // how far toward red
                        [
                            color.r * (1.0 - T) + 1.0 * T,
                            color.g * (1.0 - T) + 0.15 * T,
                            color.b * (1.0 - T) + 0.15 * T,
                        ]
                    } else {
                        [color.r, color.g, color.b]
                    };
                    let raw = InstanceRaw {
                        position: [p.x, p.y, p.z],
                        color: rgb,
                        selected: if Some(id) == selected { 1.0 } else { 0.0 },
                        scale: [scale.x, scale.y, scale.z],
                    };
                    match mesh {
                        Mesh::Cube => cube_i.push(raw),
                        Mesh::Sphere => sphere_i.push(raw),
                        Mesh::Plane => plane_i.push(raw),
                    }
                }
                let group_counts = [
                    cube_i.len() as u32,
                    sphere_i.len() as u32,
                    plane_i.len() as u32,
                ];
                let mut instances = cube_i;
                instances.extend(sphere_i);
                instances.extend(plane_i);
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
                let mut console_tab = self.console_tab;
                let log_lines = std::mem::take(&mut self.log_lines);
                let script_library = std::mem::take(&mut self.world.script_library);
                let new_script_name = std::mem::take(&mut self.new_script_name);
                let script_filter = std::mem::take(&mut self.script_filter);
                let open_script = std::mem::take(&mut self.open_script);
                // Compile-check the open script once per frame, before the egui
                // pass (the runtime lives on `self`, which the egui closure can't
                // borrow). This reflects the source as of frame start; an edit
                // made this frame shows its result next frame — the same one-frame
                // path the inspector edits use. None = no script open.
                let script_status: Option<Result<(), script::ScriptError>> = open_script
                    .as_ref()
                    .and_then(|name| script_library.get(name))
                    .map(|src| self.script_runtime.check(src));
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
                let new_selection = self.selected;
                let edited = self.selected.and_then(|id| {
                    Some((
                        id,
                        *self.world.positions.get(id)?,
                        *self.world.velocities.get(id)?,
                        self.world.colors.get(id).copied().unwrap_or_default(),
                        self.world.controlled.get(id).is_some(),
                        self.world.scales.get(id).copied().unwrap_or_default(),
                        self.world.scripts.get(id).map(|s| s.uses.clone()),
                        self.world.meshes.get(id).copied().unwrap_or_default(),
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
                let paused = self.paused;
                let mut menu_action: Option<MenuAction> = None;
                // Move the dock's per-frame state into the viewer, and lift the
                // dock layout off `self` (swapping in a throwaway) so the egui
                // closure can borrow neither. Both are drained back afterwards.
                let mut dock_state =
                    std::mem::replace(&mut self.dock_state, egui_dock::DockState::new(Vec::new()));
                let mut viewer = EditorTabViewer {
                    entity_ids,
                    selection: new_selection,
                    edited,
                    script_library,
                    new_script_name,
                    script_filter,
                    open_script,
                    script_status,
                    // Reset each frame; the Viewport tab sets it if it's visible.
                    viewport_rect: None,
                };
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
                                    ui.menu_button("File", |ui| {
                                        if ui.button("Open scene…").clicked() {
                                            menu_action = Some(MenuAction::OpenScene);
                                        }
                                        ui.separator();
                                        if ui.button("Save scene").clicked() {
                                            menu_action = Some(MenuAction::SaveScene);
                                        }
                                        if ui.button("Save scene as…").clicked() {
                                            menu_action = Some(MenuAction::SaveSceneAs);
                                        }
                                        if ui.button("Reload scene").clicked() {
                                            menu_action = Some(MenuAction::ReloadScene);
                                        }
                                        ui.separator();
                                        if ui.button("Close project").clicked() {
                                            menu_action = Some(MenuAction::CloseProject);
                                        }
                                        if ui.button("Quit").clicked() {
                                            menu_action = Some(MenuAction::Quit);
                                        }
                                    });
                                    ui.menu_button("Edit", |ui| {
                                        if ui.button("Spawn entity").clicked() {
                                            menu_action = Some(MenuAction::SpawnEntity);
                                        }
                                        if ui.button("Despawn selected").clicked() {
                                            menu_action = Some(MenuAction::DespawnSelected);
                                        }
                                        ui.separator();
                                        if ui.button("Clear selection").clicked() {
                                            menu_action = Some(MenuAction::ClearSelection);
                                        }
                                    });
                                    ui.menu_button("View", |ui| {
                                        let play_pause = if paused { "Play" } else { "Pause" };
                                        if ui.button(play_pause).clicked() {
                                            menu_action = Some(MenuAction::TogglePause);
                                        }
                                        if ui.button("Step one tick").clicked() {
                                            menu_action = Some(MenuAction::StepOnce);
                                        }
                                        ui.separator();
                                        if ui.button("Controls overlay").clicked() {
                                            menu_action = Some(MenuAction::ToggleHelp);
                                        }
                                    });
                                    ui.menu_button("Help", |ui| {
                                        if ui.button("About").clicked() {
                                            menu_action = Some(MenuAction::About);
                                        }
                                    });
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
                            // The dock fills the whole central area (below the
                            // toolbar, above the console). Frame::NONE keeps it
                            // from painting a background, so the Viewport tab —
                            // whose body we leave unpainted — shows the 3D scene
                            // rendered behind egui. This lets the Script Editor
                            // tab take over the centre, full-width.
                            egui::CentralPanel::default()
                                .frame(egui::Frame::NONE)
                                .show(ui, |ui| {
                                    egui_dock::DockArea::new(&mut dock_state)
                                        .show_inside(ui, &mut viewer);
                                });
                        });
                        state.handle_platform_output(window, full_output.platform_output);
                        let ppp = full_output.pixels_per_point;
                        let jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
                        (jobs, full_output.textures_delta, ppp)
                    } else {
                        (Vec::new(), egui::TexturesDelta::default(), 1.0)
                    };
                self.console_tab = console_tab;
                self.log_lines = log_lines;
                // Drain the dock layout and the tabs' state back onto self.
                self.dock_state = dock_state;
                self.viewport_rect = viewer.viewport_rect;
                self.world.script_library = viewer.script_library;
                self.new_script_name = viewer.new_script_name;
                self.script_filter = viewer.script_filter;
                self.open_script = viewer.open_script;
                self.selected = viewer.selection;
                let edited = viewer.edited;
                // Push any inspector edits back into the world. The render this
                // frame already used the old values; the change shows next frame
                // (same one-frame path as the keyboard nudge).
                if let Some((id, pos, vel, color, controlled, scale, script_source, mesh)) = edited
                {
                    if let Some(p) = self.world.positions.get_mut(id) {
                        *p = pos;
                    }
                    if let Some(v) = self.world.velocities.get_mut(id) {
                        *v = vel;
                    }
                    self.world.colors.insert(id, color);
                    self.world.scales.insert(id, scale);
                    self.world.meshes.insert(id, mesh);
                    if controlled {
                        self.world.controlled.insert(id, Controlled);
                    } else {
                        self.world.controlled.remove(id);
                    }
                    match script_source {
                        Some(uses) => {
                            self.world.scripts.insert(id, Script { uses });
                        }
                        None => {
                            self.world.scripts.remove(id);
                        }
                    }
                }
                // A menu item clicked this frame runs the same action method the
                // keyboard uses — one command, two triggers. This sits at
                // statement level (NOT inside the `edited` block above), so it
                // fires whether or not an entity is selected.
                match menu_action {
                    Some(MenuAction::OpenScene) => self.open_scene(),
                    Some(MenuAction::SaveScene) => self.save_scene(),
                    Some(MenuAction::SaveSceneAs) => self.save_scene_as(),
                    Some(MenuAction::ReloadScene) => self.reload_scene(),
                    Some(MenuAction::CloseProject) => self.close_project(),
                    Some(MenuAction::SpawnEntity) => self.spawn_at_focus(),
                    Some(MenuAction::DespawnSelected) => self.despawn_selected(),
                    Some(MenuAction::ClearSelection) => self.clear_selection(),
                    Some(MenuAction::TogglePause) => self.toggle_pause(),
                    Some(MenuAction::StepOnce) => self.step_once(),
                    Some(MenuAction::ToggleHelp) => self.toggle_help(),
                    Some(MenuAction::About) => {
                        self.log("Frame Editor — a hand-rolled Rust simulation engine and editor.");
                    }
                    Some(MenuAction::Quit) => event_loop.exit(),
                    None => {}
                }
                if let Some(gpu) = &mut self.gpu {
                    gpu.render(
                        &instances,
                        group_counts,
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
/// Path to the file that remembers recently opened projects (one folder path
/// per line), under the platform config directory.
fn recent_projects_file() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("frame-editor").join("recent-projects.txt"))
}

/// The remembered project folders as written, unfiltered and in file order.
fn read_recent_projects() -> Vec<std::path::PathBuf> {
    let Some(file) = recent_projects_file() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}

/// Persist the remembered project folders.
fn write_recent_projects(roots: &[std::path::PathBuf]) {
    let Some(file) = recent_projects_file() else {
        return;
    };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = roots
        .iter()
        .filter_map(|p| p.to_str())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(&file, text);
}

/// Remembered projects that still exist, most-recently-edited first (by each
/// project scene file's modification time).
/// A remembered project, resolved for display on the launcher: its folder, the
/// scene file inside it, the name (the scene file's stem), and when it was last
/// edited (the scene file's modification time).
#[derive(Clone)]
struct RecentProject {
    root: std::path::PathBuf,
    name: String,
    modified: Option<std::time::SystemTime>,
}

/// Find a project folder's scene file: the first `.ron` in it that isn't the
/// `project.ron` manifest. A project's name is this file's stem.
fn find_scene(folder: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut scenes: Vec<std::path::PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|x| x.to_str()) == Some("ron")
                && p.file_name().and_then(|n| n.to_str()) != Some("project.ron")
        })
        .collect();
    scenes.sort();
    scenes.into_iter().next()
}

/// Remembered projects that still hold a scene, most-recently-edited first.
fn sorted_recent_projects() -> Vec<RecentProject> {
    let mut projects: Vec<RecentProject> = read_recent_projects()
        .into_iter()
        .filter_map(|root| {
            let scene = find_scene(&root)?;
            let name = scene
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Project")
                .to_string();
            let modified = std::fs::metadata(&scene).and_then(|m| m.modified()).ok();
            Some(RecentProject {
                root,
                name,
                modified,
            })
        })
        .collect();
    projects.sort_by_key(|p| p.modified);
    projects.reverse();
    projects
}

/// Format a scene's last-edited time as a local date and time for the launcher.
fn format_edited(modified: Option<std::time::SystemTime>) -> String {
    match modified {
        Some(t) => chrono::DateTime::<chrono::Local>::from(t)
            .format("%b %e, %Y at %H:%M")
            .to_string(),
        None => "unknown".to_string(),
    }
}

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
    // The editor opens on the launcher screen with no project loaded, so it
    // starts from an empty world; creating or opening a project replaces it.
    let world = World::default();
    let mut app = App {
        window: None,
        gpu: None,
        world,
        mode: AppMode::Launcher,
        project_name: None,
        recent_projects: sorted_recent_projects(),
        new_project_name: String::new(),
        // No scene target until a project is opened.
        current_scene_path: None,
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
        // Default layout mirrors the old editor: the Viewport and Script Editor
        // are tabs filling the centre (click Script Editor to edit full-width
        // over the viewport), with Scene and Inspector docked on the right. All
        // of it is draggable, tabbable, and splittable at runtime.
        dock_state: {
            let mut state = egui_dock::DockState::new(vec![Tab::Viewport, Tab::Scripts]);
            state.main_surface_mut().split_right(
                egui_dock::NodeIndex::root(),
                0.78,
                vec![Tab::Scene, Tab::Inspector],
            );
            state
        },
        viewport_rect: None,
        console_tab: ConsoleTab::Output,
        log_lines: vec!["Frame Editor started.".to_string()],
        input: InputState::new(),
        new_script_name: String::new(),
        script_filter: String::new(),
        open_script: None,
        script_runtime: script::RhaiRuntime::new(),
        logo_texture: None,
    };
    println!("Frame Editor started at the launcher.");
    event_loop.run_app(&mut app).unwrap();
}
