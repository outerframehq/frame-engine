// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dense grass, made on the graphics card.
//!
//! A module hands the editor square patches of ground data ([`GrassPatch`]):
//! a grid of samples saying how high the ground is, how thick and tall the
//! grass is and what colour it is. Nothing is stored per blade. The vertex
//! shader turns each vertex number into a blade and a point on it: where the
//! blade stands comes from a low-discrepancy sequence (R2), so the first `K`
//! blades of a patch always cover it evenly, and drawing fewer blades far away
//! is just drawing fewer vertices. Each blade's height, width, turn and lean
//! come from a hash of its number, and it sways with the wind.
//!
//! The editor owns the pipeline and the quality setting ([`GrassQuality`]):
//! blades a square metre close in, and how far grass reaches. A module keeps
//! a [`GrassField`] (made from a [`GrassFactory`]) and returns it from
//! `ModuleGpu::grass`; the editor draws it after the solid geometry.

use crate::module_api::DrawView;
use wgpu::util::DeviceExt;

/// Samples per side a patch can have at most.
pub const GRASS_GRID_MAX: u32 = 33;
/// How many patches one field holds at most.
pub const GRASS_PATCHES_MAX: usize = 512;
/// Blades drawn per instance of the shared index pattern. MUST match
/// GRASS_BATCH in shader.wgsl.
const BATCH: u32 = 16_384;
/// Points per blade: three pairs up the blade and the tip. MUST match the
/// shader.
const POINTS: u32 = 7;
/// Corners per blade: five triangles.
const CORNERS: u32 = 15;
/// Bytes between patches in the uniform buffer (the largest offset alignment
/// any device asks for).
const PATCH_STRIDE: u64 = 256;
/// Floats per sample in the sample buffer: two `vec4`s.
const SAMPLE_FLOATS: usize = 8;
/// Within this distance (m) every blade a patch has is drawn; past it the
/// count falls with the square of the distance, so blades per pixel stay about
/// the same. MUST match how the shader thins blades (it reads this from the
/// settings).
pub const GRASS_FULL_NEAR: f32 = 12.0;

/// One point of a patch's grid.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GrassSample {
    /// The ground's height here, relative to the patch's anchor.
    pub height: f32,
    /// How far the ground here moves when its level of detail slides to the
    /// next coarser one (see `VertexMorph::offset_y`), so blades stay on it.
    pub coarse_offset: f32,
    /// How much of the possible grass grows here, 0 (none) to 1 (all).
    pub density: f32,
    /// How tall the grass is here, in world units (0 for none).
    pub blade_height: f32,
    /// The grass's colour here (linear 0 to 1). The blade darkens toward its
    /// root and lightens toward its tip around it, so the ground under the
    /// grass is best given the same colour.
    pub color: [f32; 3],
}

impl GrassSample {
    /// No grass at `height`.
    pub fn bare(height: f32) -> GrassSample {
        GrassSample {
            height,
            coarse_offset: 0.0,
            density: 0.0,
            blade_height: 0.0,
            color: [0.0; 3],
        }
    }
}

/// A square of ground with grass on it.
#[derive(Clone, Debug, PartialEq)]
pub struct GrassPatch {
    /// The patch's corner (lowest x and z) in world space.
    pub anchor: [f64; 3],
    /// How wide it is along x and z, in world units.
    pub size: f32,
    /// Samples per side (2 to `GRASS_GRID_MAX`), spread evenly from the
    /// anchor's corner to the far one.
    pub grid: u32,
    /// `grid * grid` samples, row by row: x first, then z.
    pub samples: Vec<GrassSample>,
    /// The distance band (from the camera, across the ground) over which the
    /// ground slides to its coarser shape, as for `VertexMorph`. `[0, 0]` for
    /// none.
    pub morph_band: [f32; 2],
    /// Varies the blades between patches so neighbours do not repeat.
    pub seed: u32,
}

impl GrassPatch {
    /// Whether it can be drawn: a sensible size and grid, and the samples to
    /// fill it.
    pub fn is_valid(&self) -> bool {
        self.size.is_finite()
            && self.size > 0.0
            && (2..=GRASS_GRID_MAX).contains(&self.grid)
            && self.samples.len() == (self.grid * self.grid) as usize
    }

    /// The lowest and highest the grass reaches, relative to the anchor.
    fn height_range(&self) -> (f32, f32) {
        self.samples
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), s| {
                let a = s.height.min(s.height + s.coarse_offset);
                let b = s.height.max(s.height + s.coarse_offset) + s.blade_height * 1.4;
                (lo.min(a), hi.max(b))
            })
    }
}

/// How much grass the editor draws: an Editor Settings choice.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GrassQuality {
    Off,
    Low,
    Medium,
    High,
    Ultra,
}

impl GrassQuality {
    pub const ALL: [GrassQuality; 5] = [
        GrassQuality::Off,
        GrassQuality::Low,
        GrassQuality::Medium,
        GrassQuality::High,
        GrassQuality::Ultra,
    ];

    /// Blades a square metre close in, and how far grass reaches (m).
    pub fn density_and_range(self) -> (f32, f32) {
        match self {
            GrassQuality::Off => (0.0, 0.0),
            GrassQuality::Low => (50.0, 60.0),
            GrassQuality::Medium => (150.0, 90.0),
            GrassQuality::High => (300.0, 120.0),
            GrassQuality::Ultra => (600.0, 150.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GrassQuality::Off => "Off",
            GrassQuality::Low => "Low",
            GrassQuality::Medium => "Medium",
            GrassQuality::High => "High",
            GrassQuality::Ultra => "Ultra",
        }
    }

    pub(crate) fn to_index(self) -> u32 {
        self as u32
    }

    pub(crate) fn from_index(i: u32) -> GrassQuality {
        *GrassQuality::ALL
            .get(i as usize)
            .unwrap_or(&GrassQuality::High)
    }
}

/// How many of a patch's blades to draw: all of them (`full`) within
/// `GRASS_FULL_NEAR` of the camera, falling with the square of the distance
/// past it, none past `range`. `distance` is from the camera to the nearest
/// point of the patch. The shader thins each blade by its own distance, so
/// this only bounds the work.
pub(crate) fn blades_to_draw(full: f32, distance: f32, range: f32) -> u32 {
    if full <= 0.0 || distance >= range {
        return 0;
    }
    let keep = (GRASS_FULL_NEAR / distance.max(GRASS_FULL_NEAR)).powi(2);
    (full * keep).ceil() as u32
}

/// The patch uniform, one per slot. MUST match GrassPatch in shader.wgsl.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct PatchRaw {
    origin: [f32; 3],
    size: f32,
    base: u32,
    grid: u32,
    seed: u32,
    flags: u32,
    band: [f32; 2],
    pad: [f32; 2],
}

/// The settings uniform, shared by every patch. MUST match GrassSettings in
/// shader.wgsl.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SettingsRaw {
    /// Blades a square metre close in.
    pub density: f32,
    /// How far grass reaches.
    pub range: f32,
    /// Within this distance every blade is drawn.
    pub near: f32,
    /// A blade's width at its root, in world units.
    pub width: f32,
    /// The wind across the ground (direction times strength, about metres a
    /// second).
    pub wind: [f32; 2],
    /// Seconds of moving wind, wrapped.
    pub time: f32,
    pub pad: f32,
    /// The render origin's x and z, wrapped, so the gusts do not jump when it
    /// moves.
    pub origin: [f32; 2],
    pub pad2: [f32; 2],
}

/// A blade's width at its root (m): chunky, for the stylised look.
pub(crate) const BLADE_WIDTH: f32 = 0.06;
/// The default breeze (direction times strength).
pub(crate) const DEFAULT_WIND: [f32; 2] = [1.0, 0.35];

/// The shared parts every field of one window uses: the bind group layout,
/// the settings uniform and the blade index pattern.
#[derive(Clone)]
pub struct GrassFactory {
    pub(crate) layout: wgpu::BindGroupLayout,
    pub(crate) settings: wgpu::Buffer,
    pub(crate) indices: wgpu::Buffer,
}

impl GrassFactory {
    pub(crate) fn new(device: &wgpu::Device) -> GrassFactory {
        let entry =
            |binding: u32, ty: wgpu::BufferBindingType, dynamic: bool| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty,
                    has_dynamic_offset: dynamic,
                    min_binding_size: None,
                },
                count: None,
            };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grass bind group layout"),
            entries: &[
                entry(
                    3,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    false,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<PatchRaw>() as u64
                        ),
                    },
                    count: None,
                },
                // The settings are read by the fragment stage too, to fade
                // far blades into the ground.
                wgpu::BindGroupLayoutEntry {
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ..entry(5, wgpu::BufferBindingType::Uniform, false)
                },
            ],
        });
        let settings = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grass settings"),
            size: std::mem::size_of::<SettingsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grass blade indices"),
            contents: bytemuck::cast_slice(&blade_indices(BATCH)),
            usage: wgpu::BufferUsages::INDEX,
        });
        GrassFactory {
            layout,
            settings,
            indices,
        }
    }
}

/// The corners of `blades` blades, each a strip of seven points (pairs up the
/// blade, then the tip) as five triangles.
fn blade_indices(blades: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity((blades * CORNERS) as usize);
    for b in 0..blades {
        let o = b * POINTS;
        for [a, c, d] in [[0, 1, 2], [1, 3, 2], [2, 3, 4], [3, 5, 4], [4, 5, 6]] {
            out.extend_from_slice(&[o + a, o + c, o + d]);
        }
    }
    out
}

/// One patch held by a field.
struct Slot {
    key: u64,
    anchor: [f64; 3],
    size: f32,
    grid: u32,
    seed: u32,
    band: [f32; 2],
    /// Lowest and highest the grass reaches, relative to the anchor.
    heights: (f32, f32),
    /// Whether any of it has grass.
    any: bool,
}

/// A module's grass for one window: up to `GRASS_PATCHES_MAX` patches, each
/// in a slot of one sample buffer, added and dropped as the module's ground
/// comes and goes. Patches are kept by a key the module chooses.
pub struct GrassField {
    samples: wgpu::Buffer,
    patches: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    indices: wgpu::Buffer,
    slots: Vec<Option<Slot>>,
    origin: [f64; 3],
}

impl GrassField {
    /// An empty field.
    pub fn new(device: &wgpu::Device, factory: &GrassFactory) -> GrassField {
        let per_patch = (GRASS_GRID_MAX * GRASS_GRID_MAX) as u64 * (SAMPLE_FLOATS as u64 * 4);
        let samples = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grass samples"),
            size: per_patch * GRASS_PATCHES_MAX as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let patches = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grass patches"),
            size: PATCH_STRIDE * GRASS_PATCHES_MAX as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grass bind group"),
            layout: &factory.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: samples.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &patches,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<PatchRaw>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: factory.settings.as_entire_binding(),
                },
            ],
        });
        GrassField {
            samples,
            patches,
            bind_group,
            indices: factory.indices.clone(),
            slots: (0..GRASS_PATCHES_MAX).map(|_| None).collect(),
            origin: [0.0; 3],
        }
    }

    /// Add or replace the patch kept as `key`. False (and nothing changes)
    /// when the patch is not valid or the field is full.
    pub fn set_patch(&mut self, queue: &wgpu::Queue, key: u64, patch: &GrassPatch) -> bool {
        if !patch.is_valid() {
            return false;
        }
        let index = match self.find(key) {
            Some(i) => i,
            None => match self.slots.iter().position(Option::is_none) {
                Some(i) => i,
                None => return false,
            },
        };
        let slot = Slot {
            key,
            anchor: patch.anchor,
            size: patch.size,
            grid: patch.grid,
            seed: patch.seed,
            band: patch.morph_band,
            heights: patch.height_range(),
            any: patch
                .samples
                .iter()
                .any(|s| s.density > 0.0 && s.blade_height > 0.0),
        };
        let floats = sample_floats(&patch.samples);
        let per_patch = (GRASS_GRID_MAX * GRASS_GRID_MAX) as u64 * (SAMPLE_FLOATS as u64 * 4);
        queue.write_buffer(
            &self.samples,
            per_patch * index as u64,
            bytemuck::cast_slice(&floats),
        );
        let raw = patch_raw(&slot, index, self.origin);
        queue.write_buffer(
            &self.patches,
            PATCH_STRIDE * index as u64,
            bytemuck::bytes_of(&raw),
        );
        self.slots[index] = Some(slot);
        true
    }

    /// Drop the patch kept as `key`, if there is one.
    pub fn remove(&mut self, key: u64) {
        if let Some(i) = self.find(key) {
            self.slots[i] = None;
        }
    }

    /// Keep only the patches whose key `keep` says yes to.
    pub fn retain(&mut self, mut keep: impl FnMut(u64) -> bool) {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|s| !keep(s.key)) {
                *slot = None;
            }
        }
    }

    /// Whether the patch kept as `key` is here.
    pub fn contains(&self, key: u64) -> bool {
        self.find(key).is_some()
    }

    /// How many patches it holds.
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// GPU memory it takes, in bytes.
    pub fn bytes(&self) -> u64 {
        self.samples.size() + self.patches.size()
    }

    /// Where the editor draws from this frame (see
    /// `ModuleGpu::set_render_origin`). Rewrites the patches' positions only
    /// when it moved.
    pub fn set_render_origin(&mut self, queue: &wgpu::Queue, origin: [f64; 3]) {
        if self.origin == origin {
            return;
        }
        self.origin = origin;
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some(slot) = slot {
                let raw = patch_raw(slot, i, origin);
                queue.write_buffer(
                    &self.patches,
                    PATCH_STRIDE * i as u64,
                    bytemuck::bytes_of(&raw),
                );
            }
        }
    }

    fn find(&self, key: u64) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.key == key))
    }

    /// Draw every patch the view can see, as many blades as its distance from
    /// `eye` (relative to the render origin) calls for. The editor has set
    /// the grass pipeline and the scene's bind group. Returns the blades
    /// drawn.
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        eye: [f32; 3],
        view: &DrawView,
        quality: GrassQuality,
    ) -> u64 {
        let (density, range) = quality.density_and_range();
        if density <= 0.0 {
            return 0;
        }
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        let mut drawn = 0u64;
        for (i, slot) in self.slots.iter().enumerate() {
            let Some(slot) = slot.as_ref().filter(|s| s.any) else {
                continue;
            };
            let o = [
                (slot.anchor[0] - self.origin[0]) as f32,
                (slot.anchor[1] - self.origin[1]) as f32,
                (slot.anchor[2] - self.origin[2]) as f32,
            ];
            // Nearest point of the patch to the eye, across the ground.
            let dx = (eye[0] - (o[0] + slot.size)).max(o[0] - eye[0]).max(0.0);
            let dz = (eye[2] - (o[2] + slot.size)).max(o[2] - eye[2]).max(0.0);
            let distance = dx.hypot(dz);
            let count = blades_to_draw(density * slot.size * slot.size, distance, range);
            if count == 0 {
                continue;
            }
            let min = [o[0], o[1] + slot.heights.0, o[2]];
            let max = [o[0] + slot.size, o[1] + slot.heights.1, o[2] + slot.size];
            if !view.sees_box(min, max) {
                continue;
            }
            pass.set_bind_group(1, &self.bind_group, &[(PATCH_STRIDE * i as u64) as u32]);
            let batches = count.div_ceil(BATCH);
            let in_one = count.min(BATCH);
            pass.draw_indexed(0..in_one * CORNERS, 0, 0..batches);
            drawn += u64::from(count);
        }
        drawn
    }
}

/// A patch's uniform for slot `index`, drawn relative to `origin`.
fn patch_raw(slot: &Slot, index: usize, origin: [f64; 3]) -> PatchRaw {
    PatchRaw {
        origin: [
            (slot.anchor[0] - origin[0]) as f32,
            (slot.anchor[1] - origin[1]) as f32,
            (slot.anchor[2] - origin[2]) as f32,
        ],
        size: slot.size,
        base: (index as u32) * GRASS_GRID_MAX * GRASS_GRID_MAX,
        grid: slot.grid,
        seed: slot.seed,
        flags: 0,
        band: slot.band,
        pad: [0.0; 2],
    }
}

/// The samples as the shader reads them: two `vec4`s each, (height, coarse
/// offset, density, blade height) and (colour, unused).
fn sample_floats(samples: &[GrassSample]) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples.len() * SAMPLE_FLOATS);
    for s in samples {
        out.extend_from_slice(&[
            s.height,
            s.coarse_offset,
            s.density.clamp(0.0, 1.0),
            s.blade_height.max(0.0),
            s.color[0],
            s.color[1],
            s.color[2],
            0.0,
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where blade `b` stands in its patch, as the shader works it out (R2,
    /// in 32-bit fixed point so far blades are as exact as near ones).
    fn spot(b: u32, seed: u32) -> [f32; 2] {
        let sx = seed.wrapping_mul(0x9e37_79b9);
        let sz = (seed ^ 0x85eb_ca6b).wrapping_mul(0x9e37_79b9);
        let x = b.wrapping_mul(3_242_174_889).wrapping_add(sx) >> 8;
        let z = b.wrapping_mul(2_447_445_413).wrapping_add(sz) >> 8;
        [x as f32 / 16_777_216.0, z as f32 / 16_777_216.0]
    }

    #[test]
    fn the_first_blades_of_a_patch_always_cover_it_evenly() {
        for count in [64u32, 1_000, 20_000] {
            // Every cell of a grid with about four blades each gets some.
            let side = ((count / 4) as f32).sqrt() as usize;
            let mut cells = vec![0u32; side * side];
            for b in 0..count {
                let [x, z] = spot(b, 7);
                let (i, k) = (
                    ((x * side as f32) as usize).min(side - 1),
                    ((z * side as f32) as usize).min(side - 1),
                );
                cells[k * side + i] += 1;
            }
            let empty = cells.iter().filter(|&&c| c == 0).count();
            assert!(
                empty * 50 <= cells.len(),
                "{count} blades: {empty} of {} cells empty",
                cells.len()
            );
        }
    }

    #[test]
    fn blades_thin_with_distance_and_stop_at_the_range() {
        let full = 300.0 * 32.0 * 32.0;
        assert_eq!(blades_to_draw(full, 0.0, 120.0), full as u32);
        assert_eq!(blades_to_draw(full, GRASS_FULL_NEAR, 120.0), full as u32);
        let at_24 = blades_to_draw(full, 2.0 * GRASS_FULL_NEAR, 120.0);
        assert!((at_24 as f32 - full / 4.0).abs() <= 1.0);
        assert_eq!(blades_to_draw(full, 120.0, 120.0), 0);
        assert_eq!(blades_to_draw(0.0, 1.0, 120.0), 0);
    }

    #[test]
    fn each_blade_is_five_triangles_of_its_own_seven_points() {
        let idx = blade_indices(3);
        assert_eq!(idx.len(), 3 * CORNERS as usize);
        for (b, tri) in idx.chunks(CORNERS as usize).enumerate() {
            let lo = b as u32 * POINTS;
            assert!(tri.iter().all(|&i| (lo..lo + POINTS).contains(&i)));
            // Every point is used.
            for p in lo..lo + POINTS {
                assert!(tri.contains(&p));
            }
        }
    }

    #[test]
    fn a_patch_must_have_a_full_grid_of_samples() {
        let mut p = GrassPatch {
            anchor: [0.0; 3],
            size: 32.0,
            grid: 3,
            samples: vec![GrassSample::bare(0.0); 9],
            morph_band: [0.0; 2],
            seed: 1,
        };
        assert!(p.is_valid());
        p.grid = 4;
        assert!(!p.is_valid());
        p.grid = GRASS_GRID_MAX + 1;
        p.samples = vec![GrassSample::bare(0.0); (p.grid * p.grid) as usize];
        assert!(!p.is_valid());
    }

    #[test]
    fn the_patch_box_holds_the_tallest_blade_and_the_slide() {
        let mut s = GrassSample::bare(2.0);
        s.coarse_offset = -1.5;
        s.blade_height = 0.5;
        s.density = 1.0;
        let p = GrassPatch {
            anchor: [0.0; 3],
            size: 8.0,
            grid: 2,
            samples: vec![s; 4],
            morph_band: [0.0; 2],
            seed: 1,
        };
        let (lo, hi) = p.height_range();
        assert!(lo <= 0.5 && hi >= 2.0 + 0.5);
    }

    #[test]
    fn uniforms_match_the_shader_sizes() {
        assert_eq!(std::mem::size_of::<PatchRaw>(), 48);
        assert_eq!(std::mem::size_of::<SettingsRaw>(), 48);
        assert!(std::mem::size_of::<PatchRaw>() as u64 <= PATCH_STRIDE);
    }

    #[test]
    fn quality_round_trips_through_its_index() {
        for q in GrassQuality::ALL {
            assert_eq!(GrassQuality::from_index(q.to_index()), q);
        }
        assert_eq!(GrassQuality::from_index(99), GrassQuality::High);
    }
}
