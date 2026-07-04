// The Rhai-backed script runtime: the concrete implementation of the engine's
// `ScriptRuntime` trait, and the only place Rhai is used.

use std::collections::HashMap;

use frame_engine::world::{Script, ScriptRuntime, World};

pub struct RhaiRuntime {
    engine: rhai::Engine,
    compiled: HashMap<String, rhai::AST>,
    // Tick counter, advanced once per tick in begin_tick and exposed to scripts
    // as `t`. A count, not wall-clock time, so it stays deterministic.
    time: f64,
}

impl RhaiRuntime {
    pub fn new() -> Self {
        let engine = rhai::Engine::new();
        let mut compiled = HashMap::new();

        // Orbit the origin in the XY plane — shows off position writes plus the
        // `t` clock. Try swapping in the pulse below to see scale animate too.
        let orbit = "px = cos(t * 0.08) * 50.0; py = sin(t * 0.08) * 50.0;";
        // let pulse = "let s = 1.0 + sin(t * 0.1) * 0.5; sx = s; sy = s; sz = s;";
        compiled.insert(
            "spinner".to_string(),
            engine
                .compile(orbit)
                .expect("built-in script should compile"),
        );

        Self {
            engine,
            compiled,
            time: 0.0,
        }
    }
}

impl ScriptRuntime for RhaiRuntime {
    fn begin_tick(&mut self) {
        self.time += 1.0;
    }

    fn run(&mut self, world: &mut World, entity: usize) {
        let name = match world.scripts.get(entity) {
            Some(script) => script.name.clone(),
            None => return,
        };
        let Some(ast) = self.compiled.get(&name) else {
            return;
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
