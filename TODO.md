- [ ] make handle block size configurable
  - even just internally, use it to test handle block realloc behavior
- [ ] inline all the move_to and accept machinery
  - small functions like that should be inlined
- [ ] add LocalMut which allows reusing a handle for different HeapRefs
  - different from `EscapeSlot`, it doesn't allow moving between scopes of different lifetimes or anything similar
    the `'a` in `LocalMut<'a, T>` will be invariant just like `Local<'a, T>`, which means it may only be used to
    store heap refs originating in that scope, whether they were moved in or not.
    It's allocating a new handle is not completely free, and if you have a loop where you allocate a bunch, it may
    be better to instead reuse the same slot. 
- [ ] add a thread_local that holds a reference to the current GC, which should be used to ensure that no other GC is created and used while another one is live. this ensures that values can't be "leaked" outside of their GC.
- [ ] write derive macro for deriving `Trace` and `Escape`
  - these should never be implemented manually outside of niche use cases, because doing it incorrectly means
    undefined behavior. that's why both the trait and fn inside are `unsafe`.
    having a proc macro do this for the user is essential.
