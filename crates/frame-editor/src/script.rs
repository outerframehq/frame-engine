// The Rhai-backed script runtime: the concrete implementation of the engine's
// `ScriptRuntime` trait, and the only place Rhai is used.

use std::collections::{HashMap, HashSet};

use frame_engine::world::{ScriptRuntime, World};

/// A 3D vector exposed to scripts as `Vec3`, with `.x` / `.y` / `.z`. Used for
/// position, velocity, and scale, so a script can say `pos.x` and do vector maths
/// (`pos = pos + vel * 2.0`) instead of juggling loose numbers.
///
/// Scripts work in `f64` (Rhai's float); the conversion to the world's `f32`
/// happens at the boundary, in `run`.
#[derive(Clone, Copy)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// A colour exposed to scripts as `Rgb`, with `.r` / `.g` / `.b`, each 0.0–1.0.
#[derive(Clone, Copy)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

/// Every name a script may use without declaring it. The semantic check treats
/// anything else that isn't a local `let` as a mistake, so this list *is* the
/// script vocabulary — keep it in step with the scope built in `run`, and with
/// SCRIPTING.md.
const API_VARS: &[&str] = &[
    // Read-only context.
    "t", "hit", // Structured values (preferred).
    "pos", "vel", "scale", "color", // Flat values (the original spelling, still supported).
    "px", "py", "pz", "dx", "dy", "dz", "sx", "sy", "sz", "cr", "cg", "cb",
];

/// A problem found in a script's source, shaped for the editor to display: a
/// human-readable message plus the 1-based line and column, if they're known.
/// Used for both hard syntax errors (`check`) and softer warnings (`warnings`).
#[derive(Clone)]
pub struct ScriptError {
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

/// Teach a Rhai engine the script API's types: `Vec3`, `Rgb`, their fields,
/// constructors, and the arithmetic that makes them worth having.
fn register_api(engine: &mut rhai::Engine) {
    engine
        .register_type_with_name::<Vec3>("Vec3")
        .register_fn("vec3", |x: f64, y: f64, z: f64| Vec3 { x, y, z })
        .register_get_set("x", |v: &mut Vec3| v.x, |v: &mut Vec3, n: f64| v.x = n)
        .register_get_set("y", |v: &mut Vec3| v.y, |v: &mut Vec3, n: f64| v.y = n)
        .register_get_set("z", |v: &mut Vec3| v.z, |v: &mut Vec3, n: f64| v.z = n)
        .register_fn("+", |a: Vec3, b: Vec3| Vec3 {
            x: a.x + b.x,
            y: a.y + b.y,
            z: a.z + b.z,
        })
        .register_fn("-", |a: Vec3, b: Vec3| Vec3 {
            x: a.x - b.x,
            y: a.y - b.y,
            z: a.z - b.z,
        })
        .register_fn("*", |a: Vec3, s: f64| Vec3 {
            x: a.x * s,
            y: a.y * s,
            z: a.z * s,
        })
        .register_fn("*", |s: f64, a: Vec3| Vec3 {
            x: a.x * s,
            y: a.y * s,
            z: a.z * s,
        })
        .register_fn("/", |a: Vec3, s: f64| Vec3 {
            x: a.x / s,
            y: a.y / s,
            z: a.z / s,
        })
        .register_fn("length", |a: Vec3| {
            (a.x * a.x + a.y * a.y + a.z * a.z).sqrt()
        })
        .register_fn("to_string", |a: Vec3| {
            format!("({}, {}, {})", a.x, a.y, a.z)
        });

    engine
        .register_type_with_name::<Rgb>("Rgb")
        .register_fn("rgb", |r: f64, g: f64, b: f64| Rgb { r, g, b })
        .register_get_set("r", |c: &mut Rgb| c.r, |c: &mut Rgb, n: f64| c.r = n)
        .register_get_set("g", |c: &mut Rgb| c.g, |c: &mut Rgb, n: f64| c.g = n)
        .register_get_set("b", |c: &mut Rgb| c.b, |c: &mut Rgb, n: f64| c.b = n)
        .register_fn("to_string", |c: Rgb| format!("({}, {}, {})", c.r, c.g, c.b));
}

pub struct RhaiRuntime {
    engine: rhai::Engine,
    // Compile cache, keyed by source text. Compile once, run every tick. Keying
    // by source means shared scripts compile once and EDITED scripts (new text =
    // new key) recompile themselves. `None` = a script that failed to compile,
    // cached so we don't retry it every tick.
    compiled: HashMap<String, Option<rhai::AST>>,
    time: f64,
}

impl RhaiRuntime {
    pub fn new() -> Self {
        let mut engine = rhai::Engine::new();
        register_api(&mut engine);
        Self {
            engine,
            compiled: HashMap::new(),
            time: 0.0,
        }
    }

    /// Compile-check a script's source without running it or touching the run
    /// cache. `Ok(())` means it parses; `Err` carries the first parse error with
    /// its position. This catches SYNTAX errors only — see `warnings` for the
    /// semantic pass that catches unknown variables.
    pub fn check(&self, source: &str) -> Result<(), ScriptError> {
        match self.engine.compile(source) {
            Ok(_) => Ok(()),
            Err(err) => Err(ScriptError {
                line: err.position().line(),
                column: err.position().position(),
                // err_type() prints the bare message; the position is reported
                // separately above, so we don't want compile()'s "(line N, ...)"
                // suffix duplicated here.
                message: err.err_type().to_string(),
            }),
        }
    }

    /// Semantic check: report every variable a script reads or writes that is
    /// neither part of the script API nor declared locally with `let`.
    ///
    /// This is the pass that catches a typo. Rhai is dynamically typed, so `pox =
    /// 5`, or a misspelled `hti`, is only an error at *run* time — the script
    /// silently does nothing, 30 times a second, with no clue why. Walking the
    /// compiled AST for variable references lets the editor say so as you type.
    ///
    /// Two deliberate simplifications: block scope is ignored (a `let` anywhere
    /// counts as declared everywhere), and source that doesn't parse yields no
    /// warnings — the syntax error is the real problem, and half-typed code would
    /// otherwise warn constantly. Both mean this only ever *under*-reports, so a
    /// warning it does give is a real one.
    pub fn warnings(&self, source: &str) -> Vec<ScriptError> {
        let Ok(ast) = self.engine.compile(source) else {
            return Vec::new();
        };

        let mut declared: HashSet<String> = HashSet::new();
        let mut used: Vec<(String, rhai::Position)> = Vec::new();

        ast.walk(&mut |path: &[rhai::ASTNode]| {
            match path.last() {
                // `let name = ...` — a local the script declared itself.
                Some(rhai::ASTNode::Stmt(rhai::Stmt::Var(decl, ..))) => {
                    declared.insert(decl.0.name.to_string());
                }
                // A bare variable reference. Namespace-qualified names (`a::b`)
                // aren't ours to judge, so skip those.
                Some(rhai::ASTNode::Expr(rhai::Expr::Variable(var, _, pos))) => {
                    if var.2.is_empty() {
                        used.push((var.1.to_string(), *pos));
                    }
                }
                _ => {}
            }
            true // keep walking
        });

        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for (name, pos) in used {
            if API_VARS.contains(&name.as_str()) || declared.contains(&name) {
                continue;
            }
            if !seen.insert(name.clone()) {
                continue; // report each unknown name once
            }
            out.push(ScriptError {
                line: pos.line(),
                column: pos.position(),
                message: format!("unknown variable '{name}' — not part of the script API"),
            });
        }
        out
    }
}

/// Pick the value to write back for one number. The script may have changed it
/// through the structured value (`pos.x`) or the flat one (`px`); whichever moved
/// away from what we handed in wins, with the structured spelling taking
/// precedence if somehow both did.
fn resolve(original: f64, structured: f64, flat: f64) -> f64 {
    if structured != original {
        structured
    } else {
        flat
    }
}

impl ScriptRuntime for RhaiRuntime {
    fn begin_tick(&mut self) {
        self.time += 1.0;
    }

    fn run(&mut self, world: &mut World, entity: usize) {
        // The entity references a library script by name. Resolve the name to
        // its source through the world's library. Clone both so we aren't
        // borrowing `world` while we mutate it below.
        let uses = match world.scripts.get(entity) {
            Some(script) => script.uses.clone(),
            None => return,
        };
        let source = match world.script_library.get(&uses) {
            Some(src) => src.clone(),
            None => return, // references a script that isn't in the library — skip
        };

        // Compile on first sight, cache by source. Because the key is the source
        // text, every entity using the same library script shares one compiled
        // AST, and editing that library script (new text) recompiles it once for
        // all of them. A broken script caches as None (reported once).
        if !self.compiled.contains_key(&source) {
            let compiled = match self.engine.compile(&source) {
                Ok(ast) => Some(ast),
                Err(e) => {
                    eprintln!("Script '{uses}' failed to compile: {e}");
                    None
                }
            };
            self.compiled.insert(source.clone(), compiled);
        }
        let Some(Some(ast)) = self.compiled.get(&source) else {
            return; // known-bad script
        };

        // --- Read the entity's state into the scope as script variables ---
        // Position and velocity fall back to 0 if somehow absent; scale and
        // colour use their component Defaults.
        let (px, py, pz) = match world.positions.get(entity) {
            Some(p) => (p.x as f64, p.y as f64, p.z as f64),
            None => (0.0, 0.0, 0.0),
        };
        let (vdx, vdy, vdz) = match world.velocities.get(entity) {
            Some(v) => (v.dx as f64, v.dy as f64, v.dz as f64),
            None => (0.0, 0.0, 0.0),
        };
        let mut scale = world.scales.get(entity).copied().unwrap_or_default();
        let mut color = world.colors.get(entity).copied().unwrap_or_default();
        let (sx, sy, sz) = (scale.x as f64, scale.y as f64, scale.z as f64);
        let (cr, cg, cb) = (color.r as f64, color.g as f64, color.b as f64);
        // Whether this entity is part of any overlapping pair this tick, from the
        // engine's collision system (which runs before scripts). Read-only.
        let hit = world
            .collisions
            .iter()
            .any(|&(a, b)| a == entity || b == entity);

        let mut scope = rhai::Scope::new();
        scope.push("t", self.time); // read-only context
        scope.push("hit", hit); // read-only: colliding with anything this tick

        // Structured values — the preferred spelling.
        scope.push(
            "pos",
            Vec3 {
                x: px,
                y: py,
                z: pz,
            },
        );
        scope.push(
            "vel",
            Vec3 {
                x: vdx,
                y: vdy,
                z: vdz,
            },
        );
        scope.push(
            "scale",
            Vec3 {
                x: sx,
                y: sy,
                z: sz,
            },
        );
        scope.push(
            "color",
            Rgb {
                r: cr,
                g: cg,
                b: cb,
            },
        );

        // Flat values — the original spelling, kept so existing scripts still run.
        scope.push("px", px);
        scope.push("py", py);
        scope.push("pz", pz);
        scope.push("dx", vdx);
        scope.push("dy", vdy);
        scope.push("dz", vdz);
        scope.push("sx", sx);
        scope.push("sy", sy);
        scope.push("sz", sz);
        scope.push("cr", cr);
        scope.push("cg", cg);
        scope.push("cb", cb);

        if self.engine.run_ast_with_scope(&mut scope, ast).is_err() {
            return; // a broken script leaves the entity untouched this tick
        }

        // --- Write the (possibly changed) values back into the world ---
        // Each number may have been touched through either spelling, so take
        // whichever one moved (see `resolve`). A value the script never assigned
        // still reads back exactly as we pushed it, so it stays put.
        let pos_s = scope.get_value::<Vec3>("pos").unwrap_or(Vec3 {
            x: px,
            y: py,
            z: pz,
        });
        let vel_s = scope.get_value::<Vec3>("vel").unwrap_or(Vec3 {
            x: vdx,
            y: vdy,
            z: vdz,
        });
        let scale_s = scope.get_value::<Vec3>("scale").unwrap_or(Vec3 {
            x: sx,
            y: sy,
            z: sz,
        });
        let color_s = scope.get_value::<Rgb>("color").unwrap_or(Rgb {
            r: cr,
            g: cg,
            b: cb,
        });

        if let Some(p) = world.positions.get_mut(entity) {
            let fx = scope.get_value::<f64>("px").unwrap_or(px);
            let fy = scope.get_value::<f64>("py").unwrap_or(py);
            let fz = scope.get_value::<f64>("pz").unwrap_or(pz);
            p.x = resolve(px, pos_s.x, fx) as f32;
            p.y = resolve(py, pos_s.y, fy) as f32;
            p.z = resolve(pz, pos_s.z, fz) as f32;
        }
        if let Some(vel) = world.velocities.get_mut(entity) {
            let fx = scope.get_value::<f64>("dx").unwrap_or(vdx);
            let fy = scope.get_value::<f64>("dy").unwrap_or(vdy);
            let fz = scope.get_value::<f64>("dz").unwrap_or(vdz);
            vel.dx = resolve(vdx, vel_s.x, fx) as f32;
            vel.dy = resolve(vdy, vel_s.y, fy) as f32;
            vel.dz = resolve(vdz, vel_s.z, fz) as f32;
        }

        let f_sx = scope.get_value::<f64>("sx").unwrap_or(sx);
        let f_sy = scope.get_value::<f64>("sy").unwrap_or(sy);
        let f_sz = scope.get_value::<f64>("sz").unwrap_or(sz);
        scale.x = resolve(sx, scale_s.x, f_sx) as f32;
        scale.y = resolve(sy, scale_s.y, f_sy) as f32;
        scale.z = resolve(sz, scale_s.z, f_sz) as f32;
        world.scales.insert(entity, scale);

        let f_cr = scope.get_value::<f64>("cr").unwrap_or(cr);
        let f_cg = scope.get_value::<f64>("cg").unwrap_or(cg);
        let f_cb = scope.get_value::<f64>("cb").unwrap_or(cb);
        color.r = resolve(cr, color_s.r, f_cr) as f32;
        color.g = resolve(cg, color_s.g, f_cg) as f32;
        color.b = resolve(cb, color_s.b, f_cb) as f32;
        world.colors.insert(entity, color);
    }
}
