# gc

Experimenting with a safe Rust GC design based on V8's handle scope concept.

## Current state

```rust
let gc = Gc::new();

// Uses generative lifetimes to ensure references on the stack don't escape their scope
gc.scope(|s| {
  // Values are allocated on the heap, and returned as lightweight handles
  // These handles can be freely passed around, and implement `Deref<Target = T>`.
  let a: Local<Value> = s.alloc(Value);

  // Every call to `alloc` may trigger a GC cycle, even if there are live references.
  let b: Local<Value> = s.alloc(Value);
})
```

## TODO

The API for storing GC'd references in objects is not yet implemented. It still needs some design work to make it ergonomic, but the fundamental concept _should_ be sound:

```rust
#[trace]
struct Thing<'gc> {
  value: Heap<'gc, Value>,
}

gc.scope(|s| {
  let thing = s.alloc(Thing {
    value: s.alloc(Value).into()
  });

  // `Heap` does not implement `Deref`.
  // In order to dereference a `Heap`, you must first root it in some scope:
  let value: Local<'_, Value> = thing.value.root(s);
})
```
