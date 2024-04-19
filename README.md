# gc

Experimenting with safe Rust GC designs

## Current state

```rust
let gc = Gc::new();

// Uses generative lifetimes to ensure references on the stack don't escape their scope
gc.scope(|s| {
  // Values are allocated on the heap, and returned as lightweight handles
  // These handles can be freely passed around, and implement `Deref<Target = T>`.
  let a: Handle<Value> = s.alloc(Value);

  // Every call to `alloc` may trigger a GC cycle, even if there are live references.
  let b: Handle<Value> = s.alloc(Value);
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
  let value: Handle<'_, Value> = thing.value.root(s);
})
```
