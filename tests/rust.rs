// Rust sample
pub struct Widget {
    pub id: u32,
    name: String,
}

impl Widget {
    pub fn new(id: u32, name: &str) -> Self {
        let count = 42;
        let ratio = 3.14;
        let msg = "hello";
        Self { id, name: name.to_string() }
    }
}
