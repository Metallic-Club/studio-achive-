# #462 generic Instance binding (step 5 of the class-system migration)

Branch `studio/462-generic-binding`. Design: `docs/design/class-system.md` (sections 0, 1, 4, 5, 13.2, 14 row #462, 15, 17).

## Goal
`metallic_script` exposes what the registry describes. `__index`/`__newindex` go through `HostApi`, method functions are cached
per VM by `DeclId`, `Instance.new` reads `CREATABLE`. Scripts cannot tell.

## Done
- `api/instance.rs`: generic `index_member`, `set_member`, `__namecall`, `Instance.new`. Removed `CLASS_METHODS`, the allow-list
  check, the `PVInstance` case in `IsA`, `is_service` in `Instance.new`, `ApiState::get_property`.
- `api/host_api.rs`: `LuauHost` (the `HostApi`), `Binding` (per-VM function cache, argument buffer, callbacks), `ClassSource`,
  the Luau trampolines, `CallOutcome::Wait` handling.
- `api/script_value.rs`: Luau to `ScriptValue` and back, `ScriptFunctions` (what a `FunctionRef` names).
- `api/bridge.rs`: `BRIDGED_METHODS`, the methods the registry does not declare yet, resolved to `ClassId`s once per VM.
- `api/host_methods.rs`: `HostState` holds the VM's `ApiState`; `SHARED` builds the table once.
- `metallic_core`: `instance_class` (no name lookup for the built-in registry), `ScriptValue::into_value`, an owned
  `From<Value>`. `World` and `resolve.rs` are unchanged.
- `FixtureApi` (`#[doc(hidden)]`): installs the API over a fixture registry for tests.
- Tests: `tests/generic_binding.rs`, `tests/method_allocations.rs`, `tests/fixture_classes/`, `src/api/instance/lookup_tests.rs`.
- Docs: `ARCHITECTURE.md` ("How to write a method", "Where to add things", the registry paragraph), `docs/luau-api.md`, crate docs.

## Next
Nothing in this issue once CI is green on the PR: read the `lua_*` rows of the `Performance` job, the first real `Ir` numbers.

## Where the design differs from the issue text (the design was followed)
- The missing-member text stays `X is not a valid member of Part`, without the full name: section 14 and 5.5 keep today's texts
  until the owner's OK (section 17).
- `api/instance.rs` keeps class-name literals for #463 and #464; `tests/generic_binding.rs` lists them with exact counts, so a new
  or stale one fails.

## Decisions where the design left room
- **The bridge is a table, not declarations.** Declaring the methods in class files would change `registry_hash`, and
  `registry_hash_is_pinned` must stay unedited; moving them is #465-#467 anyway. Each method has one owner, resolved to a
  `ClassId`, and `is_a` replaces the allow-list scan. A test fails when a class file declares a method still on the list; a body
  with no entry, or an entry with no body, fails VM setup.
- **`CurrentCamera`, `ViewportSize`, `CameraType` arms stay** next to the five the design names for #464: they are the same kind
  of computed property, added after the design was written. They are on the list as #464's.
- **Lookup order is not observable today**, because no class has a registry member named like a bridged method or an event
  (`lookup_tests.rs` holds this for every class). So moving properties ahead of methods changes nothing.
- **`Side` comes from the VM's `Context`, not `ScriptRole`:** a `Combined` host has both a server and a client VM.
- **`CallContext.script`** is the owner id of the running thread, minus one, as a handle.
- **`__namecall` is kept** (numbers below). It is a Luau closure, because a Rust caller cannot let a method yield. Its `resolve`
  reads `lua_namecallatom` and has no error guard, so it never raises: a non-function comes back as a function that raises what
  `__index` plus the VM's call would have (`attempt to call a string value`, `attempt to call missing method 'X' of Instance`).
  The atom is safe to read there: nothing runs Luau on that thread between the `NAMECALL` and `resolve`; a debugger watch runs on
  its own thread (`Scheduler::call_guarded`).
- **Argument count:** a Rust closure with a fixed tuple cannot tell nil from missing, and `MultiValue` allocates a list, so a Luau
  trampoline passes `select("#", ...)` and the first 8 arguments come by value. More than 8 falls back to a `MultiValue`.
- **Results:** `(Value, MultiValue)` pushes any number of results with no list, but always at least one. So a method declared
  `Ty::Nil` returns nothing, and an empty `Tuple` from a method declared otherwise returns one nil.
- **A destroyed argument** is named by its own last known name, a destroyed receiver by its own. Core only knows `Instance`.
- **Signals:** `index_member` has the `MemberKind::Signal` arm: a `LuaSignal` keyed `EventKind::Declared(decl)`, connected through
  the hub, never fired. `HostApi::connect_signal` answers "signals arrive with #463", as the test host in core does.
- **Callbacks:** `set_callback` stores per `(InstanceId, DeclId)` and prunes with the hub. Nothing calls one yet (#466).
- **`Instance.new("UserInputService", 5)`** must stay refused before the parent check, and three services are host objects, not
  classes. `services::is_host_object` asks `input::service` and `tags::service`; it goes when #466 and #467 make them classes.
- **`WaitRequest::Signal` and `Reply`** answer with an error naming #463 and #466: nothing can resume them yet.

## Findings
- **`__namecall` through mlua works** and saves about 28% on `lua_method_calls`. mlua's own `enable_namecall` is `#[doc(hidden)]`,
  only finds methods added through its registry and has no fallback, so it does not fit; the metamethod is added by name.
- **mlua allocates on the first borrow of a fresh Lua string:** `LuaString::to_str` clones its `ValueRef`, which creates a shared
  counter. This is the one allocation per read the #119 research left unexplained. So `part.Name` keeps its one allocation per key,
  as on the base, and a text argument has one too. A method name in `obj:Method()` is read from the VM and has none.
- **A plain `String` parameter is cheaper than borrowing:** `String::from_stack` reads the stack directly and makes no `ValueRef`.
  A borrowed-text argument type was tried for `IsA`/`FindFirstChild` and removed: `lua_method_calls` 10-17% slower than `String`.
  The same could speed up `__index` keys, but `String` coerces numbers, and `member_name` must keep refusing them.
- **`obj:Method()` allocates nothing once warm** for no arguments, numbers, or a bridged method without text
  (`tests/method_allocations.rs`).

## Behaviour check beyond the tests
A temporary example (not committed) printed, for every registry class on server and client: read, `obj[name](obj)` and `obj:Name(...)`
with 7 argument shapes for every member name and every old method and event name, writes of 12 value kinds, `IsA` of every
class, bad keys and arguments, the same after `Destroy`, and `Instance.new` of every class with bad parents; about 300,000 lines.
Base and head output are byte-identical, error texts included.

## Perf
The gate counts Callgrind `Ir` on Linux; Valgrind is not available here, so **no `Ir` was measured**: the PR's `Performance` job
gives the first real numbers. Native numbers below are `elapsed_ns` of `perf_ci_lua`, Windows 11, a working desktop with other
sessions building, not a controlled machine. Base `c007d15`, head `e9b8ad8`'s code, both `--release`, run alternately, 31 samples.

| Workload | Base min / median | Head min / median | Median change |
|---|---|---|---|
| `lua_reads` | 22.440 / 23.813 ms | 20.971 / 21.889 ms | -8.1% |
| `lua_writes` | 14.929 / 15.300 ms | 15.207 / 15.534 ms | +1.5% |
| `lua_method_calls` | 35.403 / 36.006 ms | 25.473 / 25.970 ms | -27.9% |

`lua_writes` was the one to watch: +1.5% and +2.4% in two runs of the final code (+4.3% before moving scalars directly in
`From<Value>` and `into_value`), `lua_reads` -3% to -8% across runs. `__namecall` accounts for the method calls: the same code
without it was -1.7%. `World` and the core workloads' paths were not touched, so `perf_ci` was not run.

## Commands
```
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p metallic_script -j 4 --test generic_binding --test method_allocations --lib
cargo build --release -p metallic_script --example perf_ci_lua -j 4
target/release/examples/perf_ci_lua lua_method_calls
```

## What #463 and #464 need
- **#463:** declare signals in class files; `index_member` already returns a signal object for `MemberKind::Signal`, so replace
  `EventKind::Declared` and delete `event_kind`, its call in `index_member` and `set_member`, and the context match next to it.
  Implement `HostApi::connect_signal`/`disconnect`; `api/script_value.rs` has `FunctionRef`s, `Hub` the connections.
  `WaitRequest::Signal` is refused in `parked_results`. Lower or drop the `#463` rows of `CLASS_LITERALS_LEFT`.
- **#464:** add computed access in `PropDesc`; `LuauHost::get_property`/`set_property` are where `desc.access` and `script_write`
  go. Delete the arms at the top of `index_member` and `new_index` and the `Size` clamp in `set_member`. The `Camera` and `Workspace`
  arms are listed as #464's too. `lua_to_value` keeps the text of a bad value; `readonly` is already checked in `set_property`.
- **#465-#467:** declare the method, give it a body (`host_methods.rs` or `Native`), delete its `BRIDGED_METHODS` entry and
  `methods()` function. A `Host` method's body takes over from the bridge the moment it is implemented, and
  `NOT_MIGRATED_HOST_METHODS` loses its entry. Extra arguments are refused unless `.variadic`, as #461 warned.

## Open questions
None blocking.

## Last check results (code at `e9b8ad8`, on main `78523c4`)
`cargo test --workspace -j 4 --no-fail-fast`: 2416 passed, 3 failed, 10 ignored. `metallic_render --test shading
gpu_timing_measures_a_frame_where_supported` fails alone too: the known #436. `metallic_studio --test play
script_errors_go_to_output_with_the_play_tag` passes alone (load, #497). `metallic_runtime --test camera_api ray_is_a_datatype`
hit `Script timeout: exhausted allowed execution time` under load (a wall-clock budget, #497); it passed 5 of 5 alone.
`cargo clippy --workspace --all-targets -j 4 -- -D warnings`: clean. `cargo deny check licenses`: licenses ok. `cargo fmt --all --
--check` and the class-file rustfmt step: clean. No existing test was edited; the golden `registry_hash` is unchanged.
