// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Asset loading. Mesh data and a hand rolled Wavefront OBJ parser.
//
// OBJ is plain text with one item per line. The parser covers what Blender
// exports for a plain static model:
//
//   v  x y z        vertex position
//   vn x y z        vertex normal
//   f  a b c ...    a face with 3 or more corners. Each corner is v, v/vt,
//                   v//vn or v/vt/vn. Indices are 1 based, negative counts
//                   from the end.
//
// Materials, groups and anything else are still skipped: a model's texture,
// roughness and metalness are assigned in the editor after import (see
// world::MeshMaterial), not read from a .mtl file. Texture coordinates (vt)
// ARE read now, so an imported model can be textured. Faces with more than 3
// corners get fan triangulated. A face with no normals gets a computed flat
// one so unshaded exports still light. A face with no texture coordinates
// gets (0.0, 0.0) at every corner, same "missing data gets a harmless
// default, not an error" tolerance the normal fallback already has.

/// One vertex of a parsed mesh. Position, normal and UV, same layout the
/// editor feeds the GPU. Plain data, no graphics types in the engine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshVertexData {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// Texture coordinates, (0.0, 0.0) when the OBJ had none for this corner.
    pub uv: [f32; 2],
}

/// A parsed mesh ready to render. Flat triangle list, every 3 vertices is one
/// triangle. Normalised to unit size: centred on the origin with the largest
/// half extent at 0.5, same sizing as the built in primitives. A model at
/// scale 1 comes out cube sized and keeps its own proportions. half_extents
/// is the per axis half size after that normalisation (each 0.5 or less),
/// used by collision to fit a box to the model.
#[derive(Clone, Debug)]
pub struct MeshData {
    pub vertices: Vec<MeshVertexData>,
    pub half_extents: [f32; 3],
}

/// Parse Wavefront OBJ text into a unit normalised MeshData.
///
/// Errors are strings for a log line, saying which line failed and why. Bad
/// indices are an error, not a guess. Unknown line types are skipped so real
/// exports parse.
pub fn parse_obj(text: &str) -> Result<MeshData, String> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut vertices: Vec<MeshVertexData> = Vec::new();

    for (line_no, raw_line) in text.lines().enumerate() {
        let line_no = line_no + 1; // lines count from 1 in error messages
        // Strip a trailing comment then trim.
        let line = match raw_line.find('#') {
            Some(i) => &raw_line[..i],
            None => raw_line,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let keyword = parts.next().unwrap_or("");
        match keyword {
            "v" => positions.push(parse_vec3(&mut parts, line_no, "v")?),
            "vn" => normals.push(parse_vec3(&mut parts, line_no, "vn")?),
            "vt" => uvs.push(parse_uv(&mut parts, line_no)?),
            "f" => {
                // Resolve every corner first, then fan triangulate:
                // corners (0, k, k+1) for k in 1..n-1.
                let mut corners: Vec<([f32; 3], [f32; 2], Option<[f32; 3]>)> = Vec::new();
                for token in parts {
                    let (pi, ti, ni) = parse_face_corner(token, line_no)?;
                    let position = *resolve(&positions, pi)
                        .ok_or_else(|| format!("line {line_no}: face vertex index {pi} is out of range"))?;
                    let uv = match ti {
                        Some(ti) => *resolve(&uvs, ti).ok_or_else(|| {
                            format!("line {line_no}: face texture coord index {ti} is out of range")
                        })?,
                        // No vt on this corner at all: a harmless default,
                        // same tolerance a missing normal already gets.
                        None => [0.0, 0.0],
                    };
                    let normal = match ni {
                        Some(ni) => Some(*resolve(&normals, ni).ok_or_else(|| {
                            format!("line {line_no}: face normal index {ni} is out of range")
                        })?),
                        None => None,
                    };
                    corners.push((position, uv, normal));
                }
                if corners.len() < 3 {
                    return Err(format!("line {line_no}: face has fewer than 3 corners"));
                }
                for k in 1..corners.len() - 1 {
                    let tri = [corners[0], corners[k], corners[k + 1]];
                    // A corner with no exported normal gets the flat face
                    // normal from the triangle winding.
                    let flat = face_normal(tri[0].0, tri[1].0, tri[2].0);
                    for (position, uv, normal) in tri {
                        vertices.push(MeshVertexData {
                            position,
                            normal: normal.unwrap_or(flat),
                            uv,
                        });
                    }
                }
            }
            // o, g, s, usemtl, mtllib, l, p and so on. Not used, not an error.
            _ => {}
        }
    }

    if vertices.is_empty() {
        return Err("no faces found (is this an OBJ file with geometry?)".to_string());
    }
    Ok(normalise_to_unit(vertices))
}

/// Parse three floats for a v or vn line.
fn parse_vec3<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    line_no: usize,
    what: &str,
) -> Result<[f32; 3], String> {
    let mut out = [0.0f32; 3];
    for slot in &mut out {
        let token = parts
            .next()
            .ok_or_else(|| format!("line {line_no}: '{what}' needs three numbers"))?;
        *slot = token
            .parse::<f32>()
            .map_err(|_| format!("line {line_no}: '{token}' is not a number"))?;
    }
    Ok(out)
}

/// Parse three floats for a vt line. OBJ allows a third (w) component for
/// 3D textures; this parser only ever uses 2D texture coordinates, so a
/// third number, if present, is read and discarded rather than rejected.
fn parse_uv<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    line_no: usize,
) -> Result<[f32; 2], String> {
    let mut out = [0.0f32; 2];
    for slot in &mut out {
        let token = parts
            .next()
            .ok_or_else(|| format!("line {line_no}: 'vt' needs at least two numbers"))?;
        *slot = token
            .parse::<f32>()
            .map_err(|_| format!("line {line_no}: '{token}' is not a number"))?;
    }
    Ok(out)
}

/// Parse one face corner token (v, v/vt, v//vn or v/vt/vn) into a
/// (position index, optional texture coord index, optional normal index)
/// triple. Still 1 based or negative.
fn parse_face_corner(token: &str, line_no: usize) -> Result<(i64, Option<i64>, Option<i64>), String> {
    let mut fields = token.split('/');
    let pi = fields
        .next()
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| format!("line {line_no}: bad face corner '{token}'"))?;
    let ti = match fields.next() {
        Some(s) if !s.is_empty() => Some(
            s.parse::<i64>()
                .map_err(|_| format!("line {line_no}: bad texture coord index in '{token}'"))?,
        ),
        _ => None,
    };
    let ni = match fields.next() {
        Some(s) if !s.is_empty() => Some(
            s.parse::<i64>()
                .map_err(|_| format!("line {line_no}: bad normal index in '{token}'"))?,
        ),
        _ => None,
    };
    Ok((pi, ti, ni))
}

/// Resolve an OBJ index into the list it refers to. 1 based, negative counts
/// back from the end.
fn resolve<T>(list: &[T], index: i64) -> Option<&T> {
    if index > 0 {
        list.get(index as usize - 1)
    } else if index < 0 {
        let back = (-index) as usize;
        list.len().checked_sub(back).and_then(|i| list.get(i))
    } else {
        None // index 0 is invalid in OBJ
    }
}

/// Flat normal of a triangle from its winding. Right handed, matching the
/// primitives. Degenerate triangles get an up vector instead of NaNs.
fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len <= f32::EPSILON {
        [0.0, 1.0, 0.0]
    } else {
        [n[0] / len, n[1] / len, n[2] / len]
    }
}

/// Centre the mesh on the origin and scale it uniformly so the largest half
/// extent is exactly 0.5, the same unit sizing as the primitives. Uniform
/// scale and translation leave normals alone.
fn normalise_to_unit(mut vertices: Vec<MeshVertexData>) -> MeshData {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in &vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(v.position[axis]);
            max[axis] = max[axis].max(v.position[axis]);
        }
    }
    let centre = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let mut half = [
        (max[0] - min[0]) * 0.5,
        (max[1] - min[1]) * 0.5,
        (max[2] - min[2]) * 0.5,
    ];
    let largest = half[0].max(half[1]).max(half[2]);
    // A single point or degenerate model still normalises without dividing by zero.
    let scale = if largest <= f32::EPSILON { 1.0 } else { 0.5 / largest };
    for v in &mut vertices {
        for axis in 0..3 {
            v.position[axis] = (v.position[axis] - centre[axis]) * scale;
        }
    }
    for h in &mut half {
        *h *= scale;
    }
    MeshData {
        vertices,
        half_extents: half,
    }
}
