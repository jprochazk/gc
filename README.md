# gc

Experimenting with a safe Rust GC design based on V8's handle scope concept.

## Current state

```rust
let cx = Gc::new();

// Uses generative lifetimes to ensure references on the stack don't escape their scope
cx.scope(|cx| {
  // Values are allocated on the heap, and returned as lightweight handles.
  // These handles can be freely copied, and implement `Deref<Target = T>`.
  let a: Local<Value> = cx.alloc(Value);

  // Every call to `alloc` may trigger a GC cycle, even if there are live references.
  let b: Local<Value> = cx.alloc(Value);
});
```

It's possible to place object references into other objects:
```rust
#[trace]
struct Test {
  value: u32,
}

#[trace]
struct Compound<'gc> {
  a: Heap<'gc, Test>,
}

// Compound values may only contain references stored as `Heap<T>`.
let v = cx.alloc(Compound {
  a: cx.alloc(Test { value: 100 }).to_heap(),
});

// `Heap<T>` can't be accessed directly,
// it must first be turned into a `Local` in some scope:
let a = v.a.to_local(cx);

// and now it's safe to access:
println!("{}", a.value);
```

See [this post](docs/what.md) for more information about how it works.
