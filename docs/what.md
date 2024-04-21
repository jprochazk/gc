The core idea is taken from V8, where they use something they call a "Handle" (or "Local"). There are safe bindings to V8, and they directly expose handles (e.g. `neon`, `napi-rs`, `rusty_v8` all use them in some form), so I thought that maybe the idea could be reimplemented in Rust, and it would still be sound. I worked backwards from there.

Here's how it works:
- A `Handle<T>` is a double indirection (`*mut *mut T`) under the hood, the first `*mut` pointing to a slot within a _handle block_, and the second `*mut` to the actual object.
- Handles are allocated within a _handle scope_, which are pushed/popped in a stack-based way (like a regular scope).
- A handle is live as long as its handle scope is live.
- Objects references by live handles are always safe to dereference.

A handle scope behaves like a "shadow stack". Any references to the heap live on this shadow stack instead of on the native stack. The GC treats the shadow stack as a root set. That means that if you have a `Local<T>` on the stack, it is always safe to dereference, and maybe more importantly, it is _always safe to run the GC_, even while there are live `Local<T>` on the stack, because it is guaranteed to find those references in the shadow stack. I currently have the GC run on every allocation to better exercise it in my tests.

![excalidraw illustration of the shadow stack concept. there are two columns of cells representing the stack and handles, then a larger area containing circles which represents the GC heap and the objects inside it. there are arrows pointing from some stack cells to some handle cells, and from handle cells to the objects in the GC heap](https://github.com/jprochazk/gc/assets/1665677/e9419b82-bb9a-4e2b-8bfb-c49a53a2e0a8)

That's the biggest difference from `gc-arena`, and kind of the whole point of this design: There are no sequences and no need to maintain fuel and periodically yield to the GC by returning from all native call frames. 

In V8, handle scopes are implicit, so you can call `Handle::new(value)` at any point. In Rust, the handle's lifetime is tied to its parent scope, and that lifetime is a uniquely branded invariant lifetime like the ones in `gc-arena`. That means a handle cannot be "leaked" from its scope. That actually ends up being an annoying limitation in the design, because now you can't "return" a handle to an outer scope, even though theoretically that is sound as long as you actually move the object reference from its current scope to a handle slot allocated in the outer scope. V8 solves this by having an `EscapableHandleScope`, which just before being created allocates a slot in the current scope, and then stores that slot which may be used to "escape" a value later. I added something similar, and it seems to work well.

The last part to solve was that you shouldn't be able to store `Local<T>` inside of a heap-allocated object, otherwise you could potentially leak it outside of its scope. But you need to be able to store object references inside other objects to be able to do anything interesting.
Objects are traced by the GC using a derived implementation of the `Trace` trait, and by not implementing `Trace` for `Local`, you can no longer place an object onto the GC heap if it contains a `Local`. Instead of `Local`, there is a separate type called `Heap<T>` which is a simple `*mut T` under the hood. A `Heap<T>` is never safe to dereference, and must be turned into a `Local<T>` first by placing it into some handle scope via `Heap::to_local(&scope)`. That allocates a handle slot for it and puts it there so the GC can find it, making it safe to dereference.

Here's the doubly linked list example from `gc-arena` re-implemented with this GC design: https://github.com/jprochazk/gc/blob/14340e290056d9c33ab6cc0506bc1270fef2a392/src/gc.rs#L425-L490
