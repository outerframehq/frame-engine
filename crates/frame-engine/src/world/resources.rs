//! Named, typed, per-world resources: big or fast-changing data that belongs
//! to a whole world rather than to one entity (a terrain's edits, a water
//! grid). They live in the `World`, so everything that copies the world
//! (Play, undo snapshots) copies them too, and they save and load with the
//! scene without the engine knowing their type.
//!
//! A resource is any type that is `Clone + Serialize + DeserializeOwned +
//! Send + Sync`. Loaded from a scene it stays as plain RON until something
//! asks for it by type, so a scene holding a resource nobody has loaded keeps
//! it untouched when saved again. The type's own `Serialize` decides how
//! compact the saved form is.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use std::any::Any;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// What a stored resource must be able to do. Implemented for every type
/// that is `Clone + Serialize + Send + Sync + 'static`.
pub trait Resource: Any + Send + Sync {
    fn clone_box(&self) -> Box<dyn Resource>;
    fn to_value(&self) -> Result<ron::Value, String>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Clone + Serialize + Send + Sync + 'static> Resource for T {
    fn clone_box(&self) -> Box<dyn Resource> {
        Box::new(self.clone())
    }
    fn to_value(&self) -> Result<ron::Value, String> {
        let text = ron::to_string(self).map_err(|e| e.to_string())?;
        ron::from_str(&text).map_err(|e| e.to_string())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

struct Slot {
    /// The saved form, until the resource has been read as its real type.
    stored: Option<ron::Value>,
    /// The real value once something asked for it. `Some(None)` means it was
    /// asked for and the stored form did not fit that type.
    live: OnceLock<Option<Box<dyn Resource>>>,
}

impl Slot {
    fn ensure<T: Resource + DeserializeOwned>(&self) {
        self.live.get_or_init(|| {
            let value = self.stored.clone()?;
            value
                .into_rust::<T>()
                .ok()
                .map(|t| Box::new(t) as Box<dyn Resource>)
        });
    }
}

impl Clone for Slot {
    fn clone(&self) -> Self {
        let live = OnceLock::new();
        let mut stored = self.stored.clone();
        if let Some(inner) = self.live.get() {
            if let Some(resource) = inner {
                let _ = live.set(Some(resource.clone_box()));
                stored = None;
            } else {
                let _ = live.set(None);
            }
        }
        Slot { stored, live }
    }
}

/// The world's named resources.
#[derive(Clone, Default)]
pub struct Resources {
    slots: BTreeMap<String, Slot>,
}

impl Resources {
    /// Store `value` under `name`, replacing whatever was there.
    pub fn insert<T: Resource>(&mut self, name: &str, value: T) {
        let live = OnceLock::new();
        let _ = live.set(Some(Box::new(value) as Box<dyn Resource>));
        self.slots
            .insert(name.to_string(), Slot { stored: None, live });
    }

    /// The resource named `name`, if there is one and it is a `T`.
    pub fn get<T: Resource + DeserializeOwned>(&self, name: &str) -> Option<&T> {
        let slot = self.slots.get(name)?;
        slot.ensure::<T>();
        slot.live.get()?.as_ref()?.as_any().downcast_ref::<T>()
    }

    /// The resource named `name` for changing, if there is one and it is a `T`.
    pub fn get_mut<T: Resource + DeserializeOwned>(&mut self, name: &str) -> Option<&mut T> {
        let slot = self.slots.get_mut(name)?;
        slot.ensure::<T>();
        let live = slot.live.get_mut()?.as_mut()?;
        slot.stored = None;
        live.as_any_mut().downcast_mut::<T>()
    }

    /// Whether anything is stored under `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.slots.contains_key(name)
    }

    /// Remove the resource named `name`. True if there was one.
    pub fn remove(&mut self, name: &str) -> bool {
        self.slots.remove(name).is_some()
    }

    /// Every name, in order.
    pub fn names(&self) -> Vec<String> {
        self.slots.keys().cloned().collect()
    }
}

impl Serialize for Resources {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeMap};
        let mut map = serializer.serialize_map(Some(self.slots.len()))?;
        for (name, slot) in &self.slots {
            let value = match slot.live.get() {
                Some(Some(resource)) => resource.to_value().map_err(S::Error::custom)?,
                _ => match &slot.stored {
                    Some(value) => value.clone(),
                    None => continue,
                },
            };
            map.serialize_entry(name, &value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Resources {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, ron::Value>::deserialize(deserializer)?;
        Ok(Resources {
            slots: raw
                .into_iter()
                .map(|(name, value)| {
                    (
                        name,
                        Slot {
                            stored: Some(value),
                            live: OnceLock::new(),
                        },
                    )
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::World;

    #[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
    struct Grid {
        cells: Vec<f32>,
        label: String,
    }

    fn grid() -> Grid {
        Grid {
            cells: vec![0.0, 1.5, -2.0],
            label: "water".into(),
        }
    }

    fn save_and_load(world: &World) -> World {
        let text = ron::ser::to_string_pretty(world, ron::ser::PrettyConfig::default()).unwrap();
        ron::from_str(&text).unwrap()
    }

    #[test]
    fn a_resource_reads_back_and_can_be_changed_in_place() {
        let mut world = World::default();
        world.resources.insert("grid", grid());
        assert_eq!(world.resources.get::<Grid>("grid"), Some(&grid()));
        world
            .resources
            .get_mut::<Grid>("grid")
            .unwrap()
            .cells
            .push(9.0);
        assert_eq!(world.resources.get::<Grid>("grid").unwrap().cells.len(), 4);
        assert!(world.resources.get::<Grid>("other").is_none());
    }

    #[test]
    fn a_resource_survives_a_save_and_load_and_is_live_on_first_use() {
        let mut world = World::default();
        world.resources.insert("grid", grid());
        let mut back = save_and_load(&world);
        assert!(back.resources.contains("grid"));
        assert_eq!(back.resources.get::<Grid>("grid"), Some(&grid()));
        back.resources.get_mut::<Grid>("grid").unwrap().label = "sea".into();
        let again = save_and_load(&back);
        assert_eq!(again.resources.get::<Grid>("grid").unwrap().label, "sea");
    }

    #[test]
    fn changes_made_after_loading_are_what_gets_saved() {
        let mut world = World::default();
        world.resources.insert("grid", grid());
        let mut loaded = save_and_load(&world);
        loaded
            .resources
            .get_mut::<Grid>("grid")
            .unwrap()
            .cells
            .clear();
        let reloaded = save_and_load(&loaded);
        assert!(
            reloaded
                .resources
                .get::<Grid>("grid")
                .unwrap()
                .cells
                .is_empty()
        );
    }

    #[test]
    fn a_resource_nobody_reads_is_kept_when_the_scene_is_saved_again() {
        let mut world = World::default();
        world.resources.insert("grid", grid());
        let loaded = save_and_load(&world);
        let twice = save_and_load(&loaded);
        assert_eq!(twice.resources.get::<Grid>("grid"), Some(&grid()));
    }

    #[test]
    fn a_clone_is_independent_of_the_original() {
        let mut world = World::default();
        world.resources.insert("grid", grid());
        let mut copy = world.clone();
        copy.resources.get_mut::<Grid>("grid").unwrap().cells[0] = 99.0;
        assert_eq!(world.resources.get::<Grid>("grid").unwrap().cells[0], 0.0);
        assert_eq!(copy.resources.get::<Grid>("grid").unwrap().cells[0], 99.0);
        // Also when the original had only been loaded, never read.
        let loaded = save_and_load(&world);
        let mut copy = loaded.clone();
        copy.resources.get_mut::<Grid>("grid").unwrap().cells[0] = 5.0;
        assert_eq!(loaded.resources.get::<Grid>("grid").unwrap().cells[0], 0.0);
    }

    #[test]
    fn asking_for_the_wrong_type_gives_none_and_keeps_the_data() {
        let mut world = World::default();
        world.resources.insert("grid", grid());
        let mut loaded = save_and_load(&world);
        assert!(loaded.resources.get::<Vec<u8>>("grid").is_none());
        // The data is still there for the right type, on a fresh load.
        let again = save_and_load(&loaded);
        assert_eq!(again.resources.get::<Grid>("grid"), Some(&grid()));
        assert!(loaded.resources.get_mut::<Vec<u8>>("grid").is_none());
    }

    #[test]
    fn an_older_scene_without_resources_loads_empty_and_names_and_remove_work() {
        let mut world = World::default();
        world.spawn(
            crate::world::Position {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            crate::world::Velocity {
                dx: 0.0,
                dy: 0.0,
                dz: 0.0,
            },
        );
        let text = ron::ser::to_string_pretty(&world, ron::ser::PrettyConfig::default()).unwrap();
        let older: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("resources"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut back: World = ron::from_str(&older).unwrap();
        assert!(back.resources.names().is_empty());
        back.resources.insert("a", 1u32);
        back.resources.insert("b", 2u32);
        assert_eq!(back.resources.names(), vec!["a", "b"]);
        assert!(back.resources.remove("a"));
        assert!(!back.resources.remove("a"));
        assert_eq!(back.resources.names(), vec!["b"]);
    }
}
