#![allow(dead_code)]

#[derive(gc::Trace)]
struct Foo {
    v: Vec<u8>,
}
