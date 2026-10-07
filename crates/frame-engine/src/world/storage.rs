// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
#[serde(transparent)]
pub struct ComponentStorage<T> {
    items: Vec<Option<T>>,
}

// The `Default` trait (separate from the inherent methods below) is what
// `#[serde(default)]` looks for: it lets a missing field in a scene file fall
// back to an empty storage. We implement it by hand rather than deriving it so
// it works for any `T`, even ones that aren't themselves `Default`.
impl<T> Default for ComponentStorage<T> {
    fn default() -> Self {
        ComponentStorage::new()
    }
}

impl<T> ComponentStorage<T> {
    // make a new empty storage
    pub fn new() -> Self {
        ComponentStorage { items: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Option<T>> {
        self.items.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Option<T>> {
        self.items.iter_mut()
    }

    //give entity "ID" this component growing the list if needed
    pub fn insert(&mut self, id: usize, value: T) {
        //grow the list with empty slots until its big enough to hold an index "ID"
        while self.items.len() <= id {
            self.items.push(None);
        }

        self.items[id] = Some(value);
    }

    // Remove entity "ID'S" compent (Slot becomes empty)
    pub fn remove(&mut self, id: usize) {
        if id < self.items.len() {
            self.items[id] = None;
        }
    }

    // Borrow entity "ID'S" component for reading if it exists
    pub fn get(&self, id: usize) -> Option<&T> {
        //Returns none if "ID" is out of range or slot is empty
        self.items.get(id).and_then(|slot| slot.as_ref())
    }

    // Borrow entity "ID'S" component for writing if it exists
    pub fn get_mut(&mut self, id: usize) -> Option<&mut T> {
        //Returns none if "ID" is out of range or slot is empty
        self.items.get_mut(id).and_then(|slot| slot.as_mut())
    }

    // --- Queries -----------------------------------------------------------
    //
    // Everything below is a convenience for the common system shape "do
    // something for every entity that has components A and B (and not C)".
    // They are plain iterators over the same Vec<Option<T>> data, borrow-checked
    // like any iterator, with no unsafe code and no extra storage. Each yields
    // the entity id first, then the component references.
    //
    //     // every falling entity, no hand-written index loop
    //     for (id, velocity) in world.velocities.query_mut()
    //         .with(&world.gravities)
    //         .without(&world.statics)
    //     { velocity.dy -= GRAVITY; }
    //
    // Different components live in different fields of `World`, so borrowing
    // one mutably and another immutably in the same expression is fine.

    /// True if this entity has this component.
    pub fn contains(&self, id: usize) -> bool {
        self.get(id).is_some()
    }

    /// The ids of every entity that has this component, in id order. Use this
    /// when the loop body needs the whole `World` (so no storage can stay
    /// borrowed): collect the ids first, then loop over them.
    pub fn entities(&self) -> impl Iterator<Item = usize> + '_ {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_ref().map(|_| id))
    }

    /// Every entity that has this component, as `(id, &component)`.
    pub fn query(&self) -> impl Iterator<Item = (usize, &T)> + '_ {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_ref().map(|value| (id, value)))
    }

    /// Like `query`, but the component can be changed in place.
    pub fn query_mut(&mut self) -> impl Iterator<Item = (usize, &mut T)> + '_ {
        self.items
            .iter_mut()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_mut().map(|value| (id, value)))
    }

    /// Every entity that has both this component and `other`, as
    /// `(id, &self_component, &other_component)`.
    pub fn join<'a, U>(
        &'a self,
        other: &'a ComponentStorage<U>,
    ) -> impl Iterator<Item = (usize, &'a T, &'a U)> + 'a {
        self.items
            .iter()
            .zip(other.items.iter())
            .enumerate()
            .filter_map(|(id, (a, b))| Some((id, a.as_ref()?, b.as_ref()?)))
    }

    /// Like `join`, but this storage's component can be changed in place while
    /// the other is only read. The classic "position += velocity" shape.
    pub fn join_mut<'a, U>(
        &'a mut self,
        other: &'a ComponentStorage<U>,
    ) -> impl Iterator<Item = (usize, &'a mut T, &'a U)> + 'a {
        self.items
            .iter_mut()
            .zip(other.items.iter())
            .enumerate()
            .filter_map(|(id, (a, b))| Some((id, a.as_mut()?, b.as_ref()?)))
    }

    /// Three components at once: `(id, &self, &b, &c)`.
    pub fn join3<'a, U, V>(
        &'a self,
        b: &'a ComponentStorage<U>,
        c: &'a ComponentStorage<V>,
    ) -> impl Iterator<Item = (usize, &'a T, &'a U, &'a V)> + 'a {
        self.items
            .iter()
            .zip(b.items.iter())
            .zip(c.items.iter())
            .enumerate()
            .filter_map(|(id, ((a, b), c))| Some((id, a.as_ref()?, b.as_ref()?, c.as_ref()?)))
    }

    /// Three components, with this one writable: `(id, &mut self, &b, &c)`.
    pub fn join3_mut<'a, U, V>(
        &'a mut self,
        b: &'a ComponentStorage<U>,
        c: &'a ComponentStorage<V>,
    ) -> impl Iterator<Item = (usize, &'a mut T, &'a U, &'a V)> + 'a {
        self.items
            .iter_mut()
            .zip(b.items.iter())
            .zip(c.items.iter())
            .enumerate()
            .filter_map(|(id, ((a, b), c))| Some((id, a.as_mut()?, b.as_ref()?, c.as_ref()?)))
    }
}

/// Anything a query yields starts with the entity id; this lets the `with` and
/// `without` filters below work on every query shape.
pub trait HasEntity {
    fn entity(&self) -> usize;
}
impl<A> HasEntity for (usize, A) {
    fn entity(&self) -> usize {
        self.0
    }
}
impl<A, B> HasEntity for (usize, A, B) {
    fn entity(&self) -> usize {
        self.0
    }
}
impl<A, B, C> HasEntity for (usize, A, B, C) {
    fn entity(&self) -> usize {
        self.0
    }
}

/// Keeps only entities that also have a component in `storage`. Built by
/// `QueryFilter::with`.
pub struct With<'a, I, U> {
    inner: I,
    storage: &'a ComponentStorage<U>,
}
impl<I: Iterator, U> Iterator for With<'_, I, U>
where
    I::Item: HasEntity,
{
    type Item = I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        let storage = self.storage;
        self.inner.find(|item| storage.contains(item.entity()))
    }
}

/// Keeps only entities that do NOT have a component in `storage`. Built by
/// `QueryFilter::without`.
pub struct Without<'a, I, U> {
    inner: I,
    storage: &'a ComponentStorage<U>,
}
impl<I: Iterator, U> Iterator for Without<'_, I, U>
where
    I::Item: HasEntity,
{
    type Item = I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        let storage = self.storage;
        self.inner.find(|item| !storage.contains(item.entity()))
    }
}

/// Adds `.with(&storage)` and `.without(&storage)` to every query, to filter
/// by a component the loop body doesn't need to read (usually a marker like
/// `Static`, `Gravity` or `RigidBody`).
pub trait QueryFilter: Iterator + Sized
where
    Self::Item: HasEntity,
{
    /// Keep only entities that also have this component.
    fn with<U>(self, storage: &ComponentStorage<U>) -> With<'_, Self, U> {
        With {
            inner: self,
            storage,
        }
    }
    /// Drop entities that have this component.
    fn without<U>(self, storage: &ComponentStorage<U>) -> Without<'_, Self, U> {
        Without {
            inner: self,
            storage,
        }
    }
}
impl<I: Iterator> QueryFilter for I where I::Item: HasEntity {}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage<T: Clone>(values: &[Option<T>]) -> ComponentStorage<T> {
        let mut s = ComponentStorage::new();
        for (id, v) in values.iter().enumerate() {
            if let Some(v) = v {
                s.insert(id, v.clone());
            }
        }
        s
    }

    #[test]
    fn query_skips_empty_slots_and_keeps_ids() {
        let s = storage(&[Some(1), None, Some(3)]);
        let got: Vec<_> = s.query().map(|(id, v)| (id, *v)).collect();
        assert_eq!(got, vec![(0, 1), (2, 3)]);
        assert_eq!(s.entities().collect::<Vec<_>>(), vec![0, 2]);
        assert!(s.contains(2) && !s.contains(1) && !s.contains(99));
    }

    #[test]
    fn query_mut_changes_in_place() {
        let mut s = storage(&[Some(1), None, Some(3)]);
        for (_, v) in s.query_mut() {
            *v *= 10;
        }
        assert_eq!(s.get(0), Some(&10));
        assert_eq!(s.get(1), None);
        assert_eq!(s.get(2), Some(&30));
    }

    #[test]
    fn join_needs_both_and_handles_different_lengths() {
        let a = storage(&[Some(1), Some(2), Some(3), Some(4)]);
        let b = storage(&[Some('a'), None, Some('c')]); // shorter than a
        let got: Vec<_> = a.join(&b).map(|(id, x, y)| (id, *x, *y)).collect();
        assert_eq!(got, vec![(0, 1, 'a'), (2, 3, 'c')]);
    }

    #[test]
    fn join_mut_writes_one_side_only() {
        let mut a = storage(&[Some(1), Some(2), Some(3)]);
        let b = storage(&[Some(10), None, Some(30)]);
        for (_, x, y) in a.join_mut(&b) {
            *x += *y;
        }
        assert_eq!(a.get(0), Some(&11));
        assert_eq!(a.get(1), Some(&2)); // b had nothing here, untouched
        assert_eq!(a.get(2), Some(&33));
    }

    #[test]
    fn three_way_joins() {
        let mut a = storage(&[Some(1), Some(2), Some(3)]);
        let b = storage(&[Some(1), Some(1), None]);
        let c = storage(&[Some(1), None, Some(1)]);
        let got: Vec<_> = a.join3(&b, &c).map(|(id, ..)| id).collect();
        assert_eq!(got, vec![0]);
        for (_, x, _, _) in a.join3_mut(&b, &c) {
            *x = 99;
        }
        assert_eq!(a.get(0), Some(&99));
        assert_eq!(a.get(1), Some(&2));
    }

    #[test]
    fn with_and_without_filter_by_marker() {
        let a = storage(&[Some(1), Some(2), Some(3), Some(4)]);
        let marker_yes = storage(&[Some(()), None, Some(()), Some(())]);
        let marker_no = storage(&[None, None, None, Some(())]);
        let ids: Vec<_> = a
            .query()
            .with(&marker_yes)
            .without(&marker_no)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, vec![0, 2]);
    }

    #[test]
    fn filters_work_on_joins_and_mutable_queries() {
        let mut a = storage(&[Some(1), Some(2), Some(3)]);
        let b = storage(&[Some(1), Some(1), Some(1)]);
        let skip = storage(&[None, Some(()), None]);
        for (_, x, _) in a.join_mut(&b).without(&skip) {
            *x = 0;
        }
        assert_eq!(a.get(0), Some(&0));
        assert_eq!(a.get(1), Some(&2));
        assert_eq!(a.get(2), Some(&0));
        for (_, x) in a.query_mut().with(&skip) {
            *x = 7;
        }
        assert_eq!(a.get(1), Some(&7));
    }
}
