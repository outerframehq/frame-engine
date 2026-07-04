// The Rhai-backed script runtime: the concrete implementation of the engine's
// `ScriptRuntime` trait, and the only place Rhai is used.

use std::collections::HashMap;

use frame_engine::world::{Script, ScriptRuntime, World};

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
        // Nothing is compiled here any more: behaviour is data on the world's
        // Script components, compiled on first sight in `run` below.
        Self {
            engine: rhai::Engine::new(),
            compiled: HashMap::new(),
            time: 0.0,
        }
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

        let mut scope = rhai::Scope::new();
        scope.push("t", self.time); // read-only context
        scope.push("px", px);
        scope.push("py", py);
        scope.push("pz", pz);
        scope.push("dx", vdx);
        scope.push("dy", vdy);
        scope.push("dz", vdz);
        scope.push("sx", scale.x as f64);
        scope.push("sy", scale.y as f64);
        scope.push("sz", scale.z as f64);
        scope.push("cr", color.r as f64);
        scope.push("cg", color.g as f64);
        scope.push("cb", color.b as f64);

        if self.engine.run_ast_with_scope(&mut scope, ast).is_err() {
            return; // a broken script leaves the entity untouched this tick
        }

        // --- Write the (possibly changed) values back into the world ---
        if let Some(p) = world.positions.get_mut(entity) {
            if let Some(v) = scope.get_value::<f64>("px") {
                p.x = v as f32;
            }
            if let Some(v) = scope.get_value::<f64>("py") {
                p.y = v as f32;
            }
            if let Some(v) = scope.get_value::<f64>("pz") {
                p.z = v as f32;
            }
        }
        if let Some(vel) = world.velocities.get_mut(entity) {
            if let Some(v) = scope.get_value::<f64>("dx") {
                vel.dx = v as f32;
            }
            if let Some(v) = scope.get_value::<f64>("dy") {
                vel.dy = v as f32;
            }
            if let Some(v) = scope.get_value::<f64>("dz") {
                vel.dz = v as f32;
            }
        }
        if let Some(v) = scope.get_value::<f64>("sx") {
            scale.x = v as f32;
        }
        if let Some(v) = scope.get_value::<f64>("sy") {
            scale.y = v as f32;
        }
        if let Some(v) = scope.get_value::<f64>("sz") {
            scale.z = v as f32;
        }
        world.scales.insert(entity, scale);

        if let Some(v) = scope.get_value::<f64>("cr") {
            color.r = v as f32;
        }
        if let Some(v) = scope.get_value::<f64>("cg") {
            color.g = v as f32;
        }
        if let Some(v) = scope.get_value::<f64>("cb") {
            color.b = v as f32;
        }
        world.colors.insert(entity, color);
    }
}
