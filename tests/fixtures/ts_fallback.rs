//! Fixture exercising every declaration shape the fallback must anchor.

use std::collections::HashMap;

pub const MAX_DEPTH: usize = 8;
static GLOBAL_NAME: &str = "probe";

type AliasMap = HashMap<String, usize>;

pub struct Widget {
    pub label: String,
    count: usize,
}

pub union RawParts {
    int: u32,
    float: f32,
}

pub enum Shade {
    Light,
    Dark { level: u8 },
}

pub trait Render {
    fn render(&self) -> String;
    fn hint() -> &'static str {
        "default body"
    }
}

impl Render for Widget {
    fn render(&self) -> String {
        let local = self.label.clone();
        local
    }
}

impl Widget {
    pub fn new(label: &str) -> Self {
        Widget {
            label: label.to_string(),
            count: 0,
        }
    }
}

pub fn top_level(a: usize, b: usize) -> usize {
    a + b
}

mod outer {
    pub mod inner {
        pub fn nested_fn() -> u8 {
            42
        }
    }

    pub struct Hidden;
}

macro_rules! shout {
    ($x:expr) => {
        println!("{}", $x)
    };
}

fn main() {
    shout!(top_level(1, 2));
}
