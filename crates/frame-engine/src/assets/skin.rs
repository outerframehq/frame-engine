// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Skeletons: the bones a model can be posed by, how each of its vertices
//! follows them, and the clips (walk, run) that move them over time.
//!
//! A bone has a place relative to its parent bone: moved, turned and sized
//! (`BoneTransform`). A pose is one such place for every bone; a clip says
//! how the places change over time. From a pose, each bone's place in the
//! model (`Skin::world`) follows by walking down from the bones with no
//! parent, and each bone's skinning matrix (`Skin::skinning`) moves a vertex
//! from where the model was made to where the pose puts it. A vertex follows
//! up to four bones, each by its weight (`VertexBones`), and is drawn at the
//! weighted sum of where each would take it.
//!
//! Matrices are 4 by 4, columns first (`Matrix`). Rotations are quaternions
//! written x, y, z, w. Plain data and plain maths: no graphics types, so the
//! game can read a bone's place too (to hold something in a hand, say).

/// A 4 by 4 matrix, columns first.
pub type Matrix = [f32; 16];

pub const IDENTITY: Matrix = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// A model's skeleton, its vertices' bones, and its clips.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    /// Every parent comes before its children.
    pub bones: Vec<Bone>,
    /// One for each of the model's vertices (`MeshData::vertices`).
    pub vertices: Vec<VertexBones>,
    pub clips: Vec<Clip>,
}

/// One bone.
#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    /// Its name in Blender (`hips`, `thigh.l`).
    pub name: String,
    /// Its parent's index in `Skin::bones`, always lower than its own.
    pub parent: Option<usize>,
    /// Its place relative to its parent as the model was made.
    pub rest: BoneTransform,
    /// For a bone with no parent: what it hangs from in the model (the
    /// armature's place). Unused for a bone with a parent.
    pub hangs_from: Matrix,
    /// The inverse of the bone's place in the model as made, so a vertex is
    /// first taken to where it sits relative to the bone.
    pub bind: Matrix,
}

/// A bone's place relative to its parent: scaled, then turned, then moved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoneTransform {
    pub translation: [f32; 3],
    /// A unit quaternion, x, y, z, w.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

impl Default for BoneTransform {
    fn default() -> Self {
        BoneTransform {
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
        }
    }
}

/// The bones a vertex follows and how strongly, the weights adding to 1. A
/// vertex whose weights are all 0 follows no bone and stays where it was
/// made.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VertexBones {
    pub bones: [u16; 4],
    pub weights: [f32; 4],
}

/// A named animation: how some bones' places change over `duration`
/// seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub tracks: Vec<Track>,
}

/// Which part of a bone's place a track changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    Translation,
    Rotation,
    Scale,
}

/// One bone's translation, rotation or scale over time: a value at each of
/// `times` (seconds, in order). Between two keys it moves smoothly, or with
/// `step` holds the earlier one until the next. Translations and scales use
/// the first three of each value's four numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub bone: usize,
    pub property: Property,
    pub step: bool,
    pub times: Vec<f32>,
    pub values: Vec<[f32; 4]>,
}

impl BoneTransform {
    /// As a matrix: scale, then rotate, then move.
    pub fn matrix(&self) -> Matrix {
        let [x, y, z, w] = self.rotation;
        let s = self.scale;
        let columns = [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y + z * w),
                2.0 * (x * z - y * w),
            ],
            [
                2.0 * (x * y - z * w),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z + x * w),
            ],
            [
                2.0 * (x * z + y * w),
                2.0 * (y * z - x * w),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ];
        let mut out = IDENTITY;
        for (c, column) in columns.iter().enumerate() {
            for (r, value) in column.iter().enumerate() {
                out[c * 4 + r] = value * s[c];
            }
        }
        out[12] = self.translation[0];
        out[13] = self.translation[1];
        out[14] = self.translation[2];
        out
    }

    /// Part way (`t`, 0 to 1) from `self` to `other`.
    pub fn blend(&self, other: &BoneTransform, t: f32) -> BoneTransform {
        let lerp = |a: [f32; 3], b: [f32; 3]| [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t);
        BoneTransform {
            translation: lerp(self.translation, other.translation),
            rotation: slerp(self.rotation, other.rotation, t),
            scale: lerp(self.scale, other.scale),
        }
    }
}

impl Track {
    /// The value at `time` seconds: the first key's before it starts, the
    /// last key's after it ends.
    pub fn sample(&self, time: f32) -> [f32; 4] {
        let n = self.times.len().min(self.values.len());
        if n == 0 {
            return match self.property {
                Property::Rotation => [0.0, 0.0, 0.0, 1.0],
                Property::Translation => [0.0; 4],
                Property::Scale => [1.0; 4],
            };
        }
        let next = self.times[..n].partition_point(|t| *t <= time);
        if next == 0 {
            return self.values[0];
        }
        if next >= n {
            return self.values[n - 1];
        }
        let (a, b) = (self.values[next - 1], self.values[next]);
        if self.step {
            return a;
        }
        let (t0, t1) = (self.times[next - 1], self.times[next]);
        let t = if t1 > t0 {
            ((time - t0) / (t1 - t0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        match self.property {
            Property::Rotation => slerp(a, b, t),
            _ => [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t),
        }
    }
}

impl Clip {
    /// Set `pose` (one place for each bone) to this clip at `time` seconds,
    /// for the bones and parts it moves; the rest are left as they are.
    pub fn apply(&self, time: f32, pose: &mut [BoneTransform]) {
        for track in &self.tracks {
            let Some(bone) = pose.get_mut(track.bone) else {
                continue;
            };
            let v = track.sample(time);
            match track.property {
                Property::Translation => bone.translation = [v[0], v[1], v[2]],
                Property::Rotation => bone.rotation = normalise_quaternion(v),
                Property::Scale => bone.scale = [v[0], v[1], v[2]],
            }
        }
    }
}

impl Skin {
    /// Every bone as the model was made.
    pub fn rest_pose(&self) -> Vec<BoneTransform> {
        self.bones.iter().map(|b| b.rest).collect()
    }

    /// The clip named `name`.
    pub fn clip(&self, name: &str) -> Option<&Clip> {
        self.clips.iter().find(|c| c.name == name)
    }

    /// Each bone's place in the model for `pose`. A bone missing from `pose`
    /// takes its rest place.
    pub fn world(&self, pose: &[BoneTransform]) -> Vec<Matrix> {
        let mut out: Vec<Matrix> = Vec::with_capacity(self.bones.len());
        for (i, bone) in self.bones.iter().enumerate() {
            let local = pose.get(i).unwrap_or(&bone.rest).matrix();
            let above = match bone.parent {
                Some(p) if p < i => out[p],
                _ => bone.hangs_from,
            };
            out.push(multiply(&above, &local));
        }
        out
    }

    /// Each bone's skinning matrix for `pose`: it takes a vertex from where
    /// the model was made to where the pose puts it, for the share of the
    /// vertex that follows that bone. All identity at the rest pose.
    pub fn skinning(&self, pose: &[BoneTransform]) -> Vec<Matrix> {
        self.world(pose)
            .iter()
            .zip(&self.bones)
            .map(|(world, bone)| multiply(world, &bone.bind))
            .collect()
    }

    /// Where `skinning` (from `Skin::skinning`) puts vertex `index` of the
    /// model, made at `position`. The renderer does the same on the
    /// graphics card; this is for anything else that needs to know.
    pub fn place(&self, skinning: &[Matrix], index: usize, position: [f32; 3]) -> [f32; 3] {
        let Some(v) = self.vertices.get(index) else {
            return position;
        };
        let total: f32 = v.weights.iter().sum();
        if total <= 0.0 {
            return position;
        }
        let mut out = [0.0; 3];
        for (bone, weight) in v.bones.iter().zip(v.weights) {
            if weight == 0.0 {
                continue;
            }
            let m = skinning.get(usize::from(*bone)).unwrap_or(&IDENTITY);
            let p = transform_point(m, position);
            for (o, p) in out.iter_mut().zip(p) {
                *o += p * weight / total;
            }
        }
        out
    }
}

/// `a * b`: `b` applied first, then `a`.
pub fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [0.0; 16];
    for col in 0..4 {
        for row in 0..4 {
            out[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    out
}

pub fn transform_point(m: &Matrix, p: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
}

/// The inverse of a matrix that turns, sizes and moves (its last row 0, 0,
/// 0, 1). One that squashes everything flat has none, and gives identity.
pub fn invert(m: &Matrix) -> Matrix {
    let a = |c: usize, r: usize| m[c * 4 + r];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(2, 1) * a(1, 2))
        - a(1, 0) * (a(0, 1) * a(2, 2) - a(2, 1) * a(0, 2))
        + a(2, 0) * (a(0, 1) * a(1, 2) - a(1, 1) * a(0, 2));
    if det.abs() < 1e-12 || !det.is_finite() {
        return IDENTITY;
    }
    let d = 1.0 / det;
    // The 3 by 3 inverse, by cofactors, entry (row r, column c).
    let inv = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        // Transposed cofactor: the cofactor of (c, r).
        (a(r1, c1) * a(r2, c2) - a(r2, c1) * a(r1, c2)) * d
    };
    let mut out = IDENTITY;
    for c in 0..3 {
        for r in 0..3 {
            out[c * 4 + r] = inv(r, c);
        }
    }
    let t = [m[12], m[13], m[14]];
    for r in 0..3 {
        out[12 + r] = -(0..3).map(|c| out[c * 4 + r] * t[c]).sum::<f32>();
    }
    out
}

fn normalise_quaternion(q: [f32; 4]) -> [f32; 4] {
    let l = q.iter().map(|x| x * x).sum::<f32>().sqrt();
    if l > 1e-12 && l.is_finite() {
        q.map(|x| x / l)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Part way (`t`, 0 to 1) round from rotation `a` to rotation `b`, the
/// short way, at an even speed.
pub fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut dot: f32 = (0..4).map(|i| a[i] * b[i]).sum();
    let mut b = b;
    if dot < 0.0 {
        b = b.map(|x| -x);
        dot = -dot;
    }
    let (wa, wb) = if dot > 0.9995 {
        // Nearly the same: a straight line is as good and avoids dividing
        // by almost nothing.
        (1.0 - t, t)
    } else {
        let angle = dot.clamp(-1.0, 1.0).acos();
        let s = angle.sin();
        (((1.0 - t) * angle).sin() / s, (t * angle).sin() / s)
    };
    normalise_quaternion([0, 1, 2, 3].map(|i| a[i] * wa + b[i] * wb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: &[f32], b: &[f32]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    /// A quarter turn about z.
    fn quarter_z() -> [f32; 4] {
        let h = std::f32::consts::FRAC_1_SQRT_2;
        [0.0, 0.0, h, h]
    }

    /// An arm: `upper` at the origin, `lower` one up from it. Vertex 0 at
    /// the top follows `lower`; vertex 1 at the elbow follows both evenly.
    fn arm() -> Skin {
        let up = BoneTransform {
            translation: [0.0, 1.0, 0.0],
            ..BoneTransform::default()
        };
        let mut skin = Skin {
            bones: vec![
                Bone {
                    name: "upper".to_string(),
                    parent: None,
                    rest: BoneTransform::default(),
                    hangs_from: IDENTITY,
                    bind: IDENTITY,
                },
                Bone {
                    name: "lower".to_string(),
                    parent: Some(0),
                    rest: up,
                    hangs_from: IDENTITY,
                    bind: IDENTITY,
                },
            ],
            vertices: vec![
                VertexBones {
                    bones: [1, 0, 0, 0],
                    weights: [1.0, 0.0, 0.0, 0.0],
                },
                VertexBones {
                    bones: [0, 1, 0, 0],
                    weights: [0.5, 0.5, 0.0, 0.0],
                },
            ],
            clips: vec![Clip {
                name: "bend".to_string(),
                duration: 1.0,
                tracks: vec![Track {
                    bone: 1,
                    property: Property::Rotation,
                    step: false,
                    times: vec![0.0, 1.0],
                    values: vec![[0.0, 0.0, 0.0, 1.0], quarter_z()],
                }],
            }],
        };
        let rest = skin.world(&skin.rest_pose());
        for (bone, world) in skin.bones.iter_mut().zip(rest) {
            bone.bind = invert(&world);
        }
        skin
    }

    #[test]
    fn the_rest_pose_leaves_every_vertex_where_it_was_made() {
        let skin = arm();
        let skinning = skin.skinning(&skin.rest_pose());
        for m in &skinning {
            assert!(near(m, &IDENTITY), "{m:?}");
        }
        assert_eq!(skin.place(&skinning, 0, [0.0, 2.0, 0.0]), [0.0, 2.0, 0.0]);
    }

    #[test]
    fn bending_the_lower_bone_swings_what_follows_it_round_the_elbow() {
        let skin = arm();
        let mut pose = skin.rest_pose();
        skin.clip("bend").unwrap().apply(1.0, &mut pose);
        let skinning = skin.skinning(&pose);
        // The top, one above the elbow, swings a quarter turn to one left
        // of it.
        let top = skin.place(&skinning, 0, [0.0, 2.0, 0.0]);
        assert!(near(&top, &[-1.0, 1.0, 0.0]), "{top:?}");
        // The elbow itself, half on each bone, stays put: it is on the
        // turning point.
        let elbow = skin.place(&skinning, 1, [0.0, 1.0, 0.0]);
        assert!(near(&elbow, &[0.0, 1.0, 0.0]), "{elbow:?}");
        // Half way through, half way round.
        let mut half = skin.rest_pose();
        skin.clips[0].apply(0.5, &mut half);
        let eighth = std::f32::consts::FRAC_PI_8;
        assert!(near(
            &half[1].rotation,
            &[0.0, 0.0, eighth.sin(), eighth.cos()]
        ));
        // A vertex with no bones stays where it was made.
        assert_eq!(skin.place(&skinning, 9, [3.0, 4.0, 5.0]), [3.0, 4.0, 5.0]);
    }

    #[test]
    fn tracks_hold_before_and_after_and_step_holds_between() {
        let mut track = Track {
            bone: 0,
            property: Property::Translation,
            step: false,
            times: vec![1.0, 2.0],
            values: vec![[0.0; 4], [4.0, 0.0, 0.0, 0.0]],
        };
        assert_eq!(track.sample(0.0)[0], 0.0);
        assert_eq!(track.sample(1.25)[0], 1.0);
        assert_eq!(track.sample(9.0)[0], 4.0);
        track.step = true;
        assert_eq!(track.sample(1.9)[0], 0.0);
        assert_eq!(track.sample(2.0)[0], 4.0);
    }

    #[test]
    fn invert_undoes_a_turn_a_size_and_a_move() {
        let m = BoneTransform {
            translation: [1.0, -2.0, 3.0],
            rotation: quarter_z(),
            scale: [2.0, 0.5, 3.0],
        }
        .matrix();
        assert!(near(&multiply(&invert(&m), &m), &IDENTITY));
        assert!(near(&multiply(&m, &invert(&m)), &IDENTITY));
        assert_eq!(invert(&[0.0; 16]), IDENTITY, "flat has no inverse");
    }

    #[test]
    fn blending_two_places_goes_the_short_way_round() {
        let a = BoneTransform::default();
        let b = BoneTransform {
            translation: [2.0, 0.0, 0.0],
            // The same quarter turn, written the other way (negated).
            rotation: quarter_z().map(|x| -x),
            scale: [3.0; 3],
        };
        let half = a.blend(&b, 0.5);
        assert_eq!(half.translation, [1.0, 0.0, 0.0]);
        assert_eq!(half.scale, [2.0; 3]);
        let eighth = std::f32::consts::FRAC_PI_8;
        assert!(near(
            &half.rotation,
            &[0.0, 0.0, eighth.sin(), eighth.cos()]
        ));
    }
}
