// storage for one kind of component

pub struct ComponentStorage<T> {
    items: Vec<Option<T>>,
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
}
