The core idea is taken from V8, where they use something they call a "Handle" (or "Local"). There are safe bindings to V8, and they directly expose handles (e.g. `neon`, `napi-rs`, `rusty_v8` all use them in some form), so I thought that maybe the idea could be reimplemented in Rust, and it would still be sound. I worked backwards from there.

The basic idea is to track references reachable from the native stack by... not storing them on the native stack, at least not directly. They are instead stored in a separate _shadow stack_, which contains _only_ references to managed objects. Access to managed objects is gated behind `Local<T>`, also called a _handle_. Because the GC can walk through this shadow stack to find all references which may be reachable from the native stack, it is always safe to access an object through a handle.

Handles are `*mut *mut T` under the hood, where `T` is some managed object. Handles point to a slot in a _handle block_, and the slot contains a reference to the _managed object_. These handle blocks are managed by _handle scopes_. The most recently created handle scope is also the currently _active_ handle scope. All handle scopes share the same backing storage consisting of one or more handle blocks, and only the currently active handle scope may allocate new handles. A handle is considered _live_ as long until its handle scope is dropped.

The borrow checker upholds the rules of accessing a handle scope and its handles through careful usage of `'scope` lifetimes on these types. As a result, usage of scopes and handles does not require any unsafe code, and _should_ be fully sound. Emphasis on should: The idea behind the API _is_ sound, but there may be bugs.

```rust
use gc::{Gc, Scope, Trace};

#[derive(Trace)]
struct Foo {
  v: i32,
}

let cx = &mut Gc::default()
let scope = &mut Scope::new(cx);

let foo = scope.alloc(Foo { v: 100 });
scope.collect_all(); // trigger a full collection
println!("{}", foo.v); // `foo` is still safe to access

```
