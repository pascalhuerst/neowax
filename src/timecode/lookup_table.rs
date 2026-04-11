use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Lut {
    table: HashMap<u32, i32>,
}

impl Lut {
    pub fn new(capacity: usize) -> Self {
        Self {
            table: HashMap::with_capacity(capacity),
        }
    }

    pub fn insert(&mut self, key: u32, value: i32) {
        self.table.insert(key, value);
    }

    pub fn get(&self, key: &u32) -> Option<i32> {
        self.table.get(key).copied()
    }

    pub fn contains_key(&self, key: &u32) -> bool {
        self.table.contains_key(key)
    }
} 