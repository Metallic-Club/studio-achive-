# #463 signals declared in the class registry (step 6 of the class-system migration)

Branch `studio/463-registry-signals`. Design: `docs/design/class-system.md` (sections 0, 3.2, 6, 13, 14 row #463, 15).

## Goal
Signals are class members: declared in class files with typed parameters and context, inherited, keyed by
`(InstanceId, SignalId, arg)`, fired generically, connected through `HostApi`. Scripts cannot tell, except that `Changed`,
`GetPropertyChangedSignal`, `Once` and `Wait` now exist.

## Done
- **Core:** `SignalDesc` builders (`variadic`, `context`, `parameterized`), `Ty::Signal`, `ScriptValue::Signal`, `SignalId`,
  `property_signal_arg`, `SignalQueue`, `World::fire`/`fire_key`/`signal_is_watched`/`signals_mut`, `MethodCall::fire`/`signal`,
  `World::property_changes_since`, `PropertyChange`, `ChangeCursor: PartialOrd`, `ConnectionId::allocate`.
- **Class files:** every signal scripts have today, plus `Instance.Changed` and `Instance:GetPropertyChangedSignal`.
- **Script:** removed `EventKind`, `event_kind` and its call sites and context match, `GuiEventKind::of`. Added
  `HostApi::connect_signal`/`disconnect`, `WaitRequest::Signal`, `signal:Once`, `signal:Wait`, `api/property_signals.rs`.
- **Tests:** `metallic_script/tests/registry_signals.rs`, `metallic_core/tests/signal_queue.rs`, a fingerprint unit test in
  `replication.rs`, and `tests/signal_wait.rs` and `tests/property_changed.rs` carried over from PR #412.
- **Docs:** `ARCHITECTURE.md` "How to add a signal", `docs/luau-api.md`, crate docs.

## Next
Nothing in this issue once CI is green on the PR: read the `Performance` job, the first real `Ir` numbers.

## Decisions where the design left room
- **`SignalId` is an alias of `MemberId`** and `SignalKey` keeps its #461 shape, so `HostApi` and the test hosts did not change.
- **Raising code names a signal, the class says whether it exists.** `ApiState::signal_key(instance, "Touched")` looks in the
  instance's own class in the VM's registry. A wrong-class id (`host.touched(model, hit)`) finds nothing, as before, and no table
  of class names replaces the old enum. `AttributeChanged` and `Changed` are resolved once per VM, since the logs key many
  changes and a change may be for a destroyed instance. `deliver_remote` resolves through `RemoteEvent` with no world borrow.
- **`ScriptHost::touched` and the other entry points keep the host's own input queue, not the world's.** The design says they
  enqueue the generic signal. Doing that would change behaviour: a script started in the same step must still hear the event, but
  `World::fire` only queues for keys already watched. It would also lose events while the world is borrowed, let one host take
  another's inputs on a shared world, and `remove_player` must still filter what is pending. They fire by name through the same
  `fire`. `World::fire` serves core code, drained right after the inputs. No built-in class fires through it yet.
- **Two hosts on one world share its `SignalQueue`:** the first to step takes the fires. Only `World::fire` is affected.
- **The world's watched set is kept loosely.** A key is published at once on connect or wait, and removed at the end of the step
  (`Hub::sync_published`). Listening a little too long only queues a fire that finds nobody; a busy world catches up later.
- **`arg` of `GetPropertyChangedSignal` is the property's member id plus one:** unique in the class, 0 stays the whole signal.
  `GetAttributeChangedSignal` keeps its per-VM interned names.
- **`GetPropertyChangedSignal` is a `Native` method returning `ScriptValue::Signal`,** so #474 needs nothing Luau-specific.
- **Subscribers cross `HostApi` as `FunctionRef`.** The Luau hub keeps the `Function` itself, as `set_callback` does and as
  `ScriptFunctions`' docs say: a reference only lasts for its call. A WASM host would keep its own callback id.
- **`Connect` on a signal of a destroyed instance still succeeds,** and the connection goes at the next prune, as today. So
  `LuaSignal` connects through `connect_listener`, past the liveness check `HostApi::connect_signal` has.
- **Context is checked when a signal member is read** (today's text and place) **and in `connect_signal`.** `check_input_member`
  stays until #467 and answers first for `InputAction`, whose signals are declared `Client`. GUI signals stay `Any`: a server script
  may read them today.
- **Signals are on the classes scripts see today,** `RemoteEvent` rather than the design's `BaseRemoteEvent` (`UnreliableRemoteEvent`
  has none today). GUI pointer signals are on `GuiObject`: `Frame` is its only child, so no answer changes.
- **`LastInputTypeChanged` and the two tag signals use reserved ids** on the RunService carrier, until #466 and #467 make those
  services classes. A test holds that no class reaches those ids.
- **Delivery order in a step:** remotes, inputs, world queue, input actions, attributes and tags, GUI, property changes, Heartbeat.
  For one change, `GetPropertyChangedSignal` handlers run before `Changed` handlers (#412's order).
- **`Once` and `Wait` are #412's design:** waiters resume after handlers, under the resume budget, and count as handler calls
  against `step_handler_limit`.

## Status of related issues
- **#404 (Wait, Once):** implemented and tested here. PR #412's script part is superseded.
- **#410 (Changed, GetPropertyChangedSignal):** implemented and tested here, without #412's core journal change: #411's journal
  already had what it needed.
- **#437 (Parent, CFrame):** left. They are not class properties until #464. Today they give
  `Parent is not a valid property name.` #412 had `... is not supported yet`; a class-free binding has no way to tell them apart.
- **PR #412** still holds #405 (Rapier), which this does not touch.

## Edited existing tests
- `CLASS_LITERALS_LEFT` counts and the golden `registry_hash` (signals are hashed; moved on purpose).
- `src/api/instance/lookup_tests.rs`: dropped the half of one test that called the deleted `event_kind`, as the brief requires.
  The test is renamed `no_registry_member_of_a_class_shares_a_name_with_its_bridged_methods`.

## Stopped and reported
`tests/generic_binding.rs` asserts `waiting for a signal arrives with #463` for a fixture key naming a property, not a signal. A real
signal key now parks; a key naming no signal stays refused with that exact text, because the test may not be edited. The text is
stale. To fix: reword it in `parked_results` and that test together.

## Problem on main, not fixed here
`crates/metallic_script/tests/enums.rs:269` uses `common::TEST_RESUME_BUDGET`, which #513 removed, so at `20d79fa` that test target
does not compile and `cargo test`/`clippy --all-targets` stop. Checks here ran with a local, uncommitted one-word change to
`metallic_script::TEST_RESUME_BUDGET`. Do not commit it from this branch; main needs its own fix.

## Behaviour check beyond the tests
A temporary example (deleted, never committed) printed, base `20d79fa` against head, on server and client: read, write and call of 28
signal names on every creatable class and service, bad `Connect` arguments, `Disconnect`, children named like signals, destroyed
instances, and a firing run: touches (dead hit, dead source, wrong classes), deaths, characters, players joining and leaving, remotes,
actions, last input type, GUI, attributes, tags, handler errors, connect and disconnect inside handlers, step limits, restart, plus
the count of handler arguments built after each step. 8,275 lines on base, 8,793 on head. Every differing line is about `Changed` or
`GetPropertyChangedSignal`; the firing section is byte-identical. `Once` and `Wait` do not exist on base, so tests cover them.

## Perf
See the numbers in the last section of this note. No `Ir` was measured: Valgrind is not available here, so the PR's `Performance`
job gives the first real ones. `World::set` has no changed line, and no write path reads the signal queue.

## Commands
```
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p metallic_core -j 4 --test signal_queue --lib
cargo test -p metallic_script -j 4 --test registry_signals --test signal_wait --test property_changed --test generic_binding
cargo run -q -p metallic_core --example registry_hash
```

## What #464 and #465-#467 need
- **#464:** when `Parent` and `CFrame` become computed properties, `GetPropertyChangedSignal` starts accepting them with no change,
  but they will never fire: the journal has `Reparented`, not `Property("Parent")`. Map those in `deliver_property_changes` (that is
  #437) in the same step, or the signal is silently dead. `CLASS_LITERALS_LEFT` is now #464's alone.
- **#465:** `GetAttributeChangedSignal` can become a method returning `ScriptValue::Signal`, like `GetPropertyChangedSignal`. It
  still needs the per-VM name table, so it is a `Host` method.
- **#466:** `RemoteEvent` signals can move to `BaseRemoteEvent` only if `UnreliableRemoteEvent` should have them. When
  `CollectionService` and `UserInputService` become classes, declare their signals and delete the reserved ids in `api/signals.rs`.
  `WaitRequest::Reply` is still refused.
- **#467:** drop `check_input_member`: the `Client` context on `InputAction`'s signals takes over, but its text differs, so check the
  tests. `HostEvent` could then be reconsidered too.
- **#468:** `check_signal_context` is the signal half already; the same shape fits properties and methods.
- **#474:** `connect_signal`, `disconnect` and `ScriptValue::Signal` are what WIT needs. `Once` and `Wait` are Luau-side only today;
  `WaitRequest::Signal` is the neutral form of `Wait`.
