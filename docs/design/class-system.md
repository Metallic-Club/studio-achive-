# Class system: design spec

Date: 2026-10-31. Status: **planned** (docs only; nothing here is implemented). Issue #459, epic #458.

One class registry describes every class scripts see: its superclass, flags, properties, methods, signals and
callbacks. Subclasses inherit all of it. The Luau binding, `Instance.new`, scene files, replication, Studio, the API
docs, the Luau type definitions and the other languages of #25 read that one description. This spec builds on:

- `crates/metallic_core/src/classes.rs` and `replication.rs`: the registry and its fingerprint today (section 1).
- `crates/metallic_script/src/api/`: the hand-wired Luau API that this design replaces.
- `docs/performance-ci.md`, `docs/research/2026-10-30-luau-property-reads.md` (#119) and
  `docs/research/2026-10-30-compact-property-storage.md` (#187): the perf gate and what was measured.
- The spec's "Code in the language you know" (one API definition, WIT generated from the registry, `wasmtime` for
  C#, Rust, Python and Java) and "Server and client from the start".
- The owner's earlier engine Forge (Bevy-based, Luau only), read as a reference: its `ClassDesc` builder, resolved
  per-class tables, `MethodCall`/`ScriptValue` calling convention, computed properties and `build.rs` discovery.
  This design keeps those ideas and changes five things: typed method signatures, explicit `overrides`, an execution
  context per member, a language-neutral host interface, and stable ids covered by `registry_hash`.

Labels: **Decision** is made here, with a one-line reason; the owner can overturn it (section 17). **Recommended**
marks a choice a follow-up issue makes final. **(Estimate)** means not measured. **(Unverified)** means I could not
confirm it while writing. All code below is the **target API, illustrative, not compiled**; names may change in the
implementing PR, the shape should not.

## 0. Decisions in short

1. **One `ClassDesc` per class, one file per class**, under `crates/metallic_core/src/classes/<group>/<class>.rs`,
   discovered by `build.rs`. Adding a class touches no other file.
2. **Members are properties, methods, signals and callbacks**, all inherited. Methods and signals have typed
   signatures; the binding checks arguments from the signature.
3. **Redeclaring an inherited member is an error unless it says `overrides()`**; `defaults` and `hide` change an
   inherited property without redeclaring it. A test proves each rule.
4. **No shared member sets now.** A real abstract `PVInstance` carries the pivot API. Member sets can be added later
   without changing ids (section 3.5).
5. **Abstract classes are inserted, never reparented**: `PVInstance`, `GuiBase2d`, `GuiObject`, `LuaSourceContainer`,
   `BaseScript`, `BaseRemoteEvent`, `ServiceProvider`, `DataModel`. Every existing `IsA` answer stays the same.
6. **Ids**: `ClassId(u16)` and a class-relative `MemberId(u16)` in base-first order (the **prefix rule**: an inherited
   member has the same id in every subclass). World nodes store a `ClassId`, not a class name.
7. **Ids are never persisted.** Scenes save names; replication compares `registry_hash` first; WASM scripts resolve
   names to ids once at load. So class order can be simply "sorted by name".
8. **`registry_hash` v5** covers flags, every member and its signature, context and enums; not descriptions or editor
   metadata.
9. **Core stays free of mlua.** Methods are `fn(&mut MethodCall) -> Result<CallOutcome, ApiError>`. Yielding is a
   return value (`CallOutcome::Wait`), not a Luau feature, so every language can yield.
10. **Methods that need the script host are declared in core and implemented in `metallic_script`**, registered by id;
    a test fails if one is missing.
11. **Signals are keyed by `(InstanceId, SignalId, arg)`**, queued in core, delivered deferred as today.
12. **Computed properties** replace every special case in `__index`/`__newindex`.
13. **Execution context per member** (`Any`, `Server`, `Client`; later `Studio`, `Plugin`), checked in the host
    interface below every language.
14. **Existing error texts are kept during the migration.** Tests stay unchanged; unifying the texts is one later PR
    with the owner's OK (section 17).
15. **One host interface addressed by ids** (`get_property`, `set_property`, `call_method`, `connect_signal`, ...).
    The Luau binding is its first user; mlua and wasmtime both go through it.
16. **WIT is a small generic interface plus generated typed classes per language** (Recommended), not one WIT resource
    per class.
17. **Generated, committed, CI-checked outputs**: `docs/api/reference.md`, `metallic.d.luau`, `api-dump.json`,
    `metallic.wit`, golden bindings.

## 1. Why: what exists today

Only properties are inherited. Everything else is matched on class names in `metallic_script`.

| Problem | Where (main `e8fa980`) |
|---|---|
| The registry is one 42-class array; adding a class means a second edit in a hand-kept `match` whose order must not drift | `metallic_core/src/classes.rs:249-757` (`CLASSES`), `:783-830` (`class_slot`) |
| Every property read walks the class chain by name (`class_slot` per step, linear scan per class) | `classes.rs:860-869` (`find_property`), called from `World::get`/`get_ref`/`set` (`instance.rs:603-660`) |
| `Name` defaults to the class name through a special case | `classes.rs:871-907` |
| No flags: abstract, creatable, service, deprecated do not exist (#93) | `ClassDef` at `classes.rs:159-166` |
| Enumerations are plain string lists | `classes.rs:11-71` (`MATERIALS`, `SHAPES`, `INPUT_*`, `LIGHTING_STYLES`, `TONEMAPPER_PRESETS`) |
| Methods are one flat table plus a `(method, class)` allow-list; no inheritance, `GetPivot` and `PivotTo` listed twice | `metallic_script/src/api/instance.rs:28-51` (`CLASS_METHODS`), `:338-345` |
| `PVInstance` is faked inside `IsA` | `instance.rs:498-502` |
| Events are a closed enum matched on class names | `api/signals.rs:35-75` (`EventKind`), `instance.rs:53-73` (`event_kind`), `api/gui.rs:45` (`GuiEventKind::of`) |
| `ClassName`, `Parent`, `CFrame`, `WorldPivot`, `LocalPlayer` are `match` arms; `Size` clamping is a special case | `instance.rs:314-334` (`index`), `:379-401` and `:411-417` (`new_index`) |
| Server/client checks are inside each method or keyed by member name | `api/remote.rs:233, 246, 273`; `api/input.rs:574-588` (`check_input_member`); `instance.rs:348-358` |
| Services are a hand list, three of them not registry classes | `api/services.rs:18-30` (`SERVICES`); `UserInputService`, `ContextActionService`, `CollectionService` are host userdata (`api/input.rs:1359`, `api/tags.rs:354`) |
| `Instance.new` refuses services through that list | `instance.rs:610` |
| Pivot methods check the class by hand | `api/pivot.rs:19-22` (`expect_pv`) |
| Studio hard-codes property sections | `metallic_studio/src/properties.rs:60-79` (`section`, `SECTION_NAMES`) |
| No methods, signals or descriptions in the registry, so no generated docs, Luau types, Litha dump (#141) or WIT (#25) | |

Each new class or method today edits several unrelated `match` statements. That does not scale to the ~600-feature
target, and other languages would have to copy every special case.

## 2. Goals and non-goals

Goals:

- One description per class; every consumer reads it.
- Adding a class or member is one file. It appears in Luau, Studio, the docs, `.d.luau`, the JSON dump and WIT.
- Nothing in the registry, the calling convention or the host interface is Luau-specific.
- Property reads, writes and method calls cost no more than today (perf gate, section 4.4).

Non-goals:

- **Physics.** `Touched`/`TouchEnded` are only *declared* on `BasePart`; `metallic_physics` and the runtime keep
  raising them through `ScriptHost::touched`/`touch_ended`.
- **No behaviour change during the migration.** Every test in `crates/metallic_script/tests` passes unchanged at every
  step. Deliberate changes (section 17) come after, each with the owner's OK.
- Behaviour hooks (Forge's `on_created`, `on_prop_changed`, render/physics components). Out of this epic.
- Compact property storage (#187). The `PropertyIndex` defined here enables it; the storage change is separate.
- Building the C#, Rust, Python and Java toolchains (#25, milestone 1D). This epic produces what they consume.
- Creator-defined classes. Section 16.3 shows the design leaves room for them.

## 3. The model

### 3.1 ClassDesc

| Field | Type | Meaning |
|---|---|---|
| `name` | `&'static str` | The class name scripts and scene files use. Unique. |
| `superclass` | `Option<&'static str>` | By name, so class files never import each other. `None` only for `Instance`. |
| `flags` | `ClassFlags` | See below. |
| `category` | `&'static str` | Insert menu and docs grouping ("Parts", "Lighting", "GUI", ...). |
| `description` | `&'static str` | One or two sentences; tooltips, reference, Litha. |
| `props`, `methods`, `signals`, `callbacks` | `&'static [..]` | Members this class declares (not inherited ones). |
| `defaults` | `&'static [(&str, fn() -> Value)]` | New defaults for inherited properties. |
| `hide` | `&'static [&str]` | Inherited members hidden for this class and its subclasses. |
| `enums` | `&'static [EnumDesc]` | Enums this class owns (section 9). |
| `luau` | `&'static str` | Optional Luau source for Luau-implemented members (section 10). |
| `context` | `MemberContext` | Default context for this class's own members (section 8). Default `Any`. |

| Flag | Meaning | Decision |
|---|---|---|
| `ABSTRACT` | Cannot be created; exists for inheritance and `IsA`. | `Instance.new` and scene load refuse it. |
| `CREATABLE` | `Instance.new` and the Insert menu may create it. | Explicit opt-in; reason: a missing flag fails closed. |
| `SERVICE` | Exactly one per game, reached by `GetService`; not creatable. | Replaces `api/services.rs` `SERVICES`. |
| `NOT_REPLICATED` | Instances of this class are never sent to clients. | Meaning only; instance/owner visibility stays in `replication.rs`. |
| `DEPRECATED` | Warn once per script on use; `deprecated_by` names the replacement. | Same rule as deprecated members (section 8). |

`creatable` and `service` cannot both be set; `abstract` excludes both (resolution error).

### 3.2 Members

Names are unique across kinds within a resolved class: a property and a method cannot share a name. Members are found
before children, as today.

**Properties** (`PropDesc`) keep every field of today's `PropDef` and add:

| Field | Kept or new | Meaning |
|---|---|---|
| `name`, `ty`, `default`, `allowed`, `range`, `normalize` + `normalize_schema_id`, `repl`, `hidden` | kept | Same meaning and same checks in `World::set`. |
| `readonly` | new | Scripts cannot write it; the engine can. |
| `access` | new | `Stored` (default) or `Computed { get, set: Option }` (section 7). |
| `saved` | new | Default true for stored, false for computed. |
| `aliases` | new | Old names that still resolve to this member (same `MemberId`). |
| `enum_type` | new | For enum-typed properties (section 9). |
| `script_write` | new | A filter every language binding applies before `World::set` (today only `BasePart.Size` clamping). Not applied by scene load, so behaviour stays as today. |
| `editor` | new | `section`, `order`, `label`, `widget` (auto, slider, drag, multiline, asset, enum, script, style picker), `visible_when`, `readonly` (in Studio; scripts are the field above). For #473. |
| `context`, `deprecated`, `description` | new | Sections 8 and 12. |

**Methods** (`MethodDesc`):

| Field | Meaning |
|---|---|
| `name`, `description` | |
| `params` | `&[Param { name, ty: Ty, optional, check }]`. `check` is `Auto` (the binding checks the type) or `Manual` (the method checks and keeps its own error text; used only during migration, section 5.5). |
| `variadic` | Trailing `...: Ty` (for `FireServer(...)`). |
| `returns` | `Ty`, `Ty::Tuple(&[..])` or `Ty::Nil`. |
| `yields` | The call may return `CallOutcome::Wait` (section 5.4). |
| `implementation` | `Native(MethodFn)` in core, `Host` (implemented in `metallic_script`, section 5.6), or `Luau` (section 10). |
| `overrides` | This replaces an inherited method of the same name and signature. |
| `context`, `deprecated` | Section 8. |

`Ty` is the language-neutral type set: `Nil`, `Bool`, `Int`, `Number`, `String`, `Instance(Option<&str>)` (optionally a
class), `Vector3`, `CFrame`, `Color3`, `UDim2`, `Enum(&str)`, `Array(&Ty)`, `Map(&Ty)`, `Function(signature)`,
`Optional(&Ty)`, `Any`, later the data types of #472. Every `Ty` has a row in the type mapping table (section 13.5).

**Signals** (`SignalDesc`): `name`, `params: &[Param]` (the payload), `context`, `parameterized` (section 6),
`description`, `deprecated`.

**Callbacks** (`CallbackDesc`): a member a script assigns a function to, such as `RemoteFunction.OnServerInvoke`.
Fields: `name`, `params`, `returns`, `context`, `description`. Reading a callback member errors (`<Name> is a
callback; it can only be set`). The host keeps the function per `(InstanceId, CallbackId)`.

### 3.3 Inheritance rules

Resolution walks the chain root first and builds one flat table per class. Lookups then never walk the chain.

| Rule | Behaviour | Test (in #460) |
|---|---|---|
| Inheritance | A class has every member of its superclass chain. | A method declared on `PVInstance` is found on `Part`, `SpawnLocation`, `Model`. |
| Self first | When an override exists, the nearest declaration wins. | `override` on a subclass is what the subclass returns; the superclass keeps its own. |
| Redeclare | Declaring a member whose name an ancestor has is a **resolution error** unless it has `overrides()`. Forge silently replaced it; we make it explicit. | Fixture registry with a bare redeclaration fails with `class X redeclares inherited member Y (add overrides())`. |
| Override shape | An override keeps the kind and the signature (methods), or the type (properties). It may change implementation, description, context only to a narrower one. | Fixture with a changed signature fails. |
| Override without base | `overrides()` with nothing to override is an error. | Fixture fails. |
| `defaults` | Changes an inherited property's default for this class and its subclasses. Naming a property that is not inherited is an error. | `default_value_ref` gives the new default on the class, the old one on the superclass. |
| `hide` | The member stays (same id, still replicated and saved) but is hidden from scripts, Studio and generated docs for this class and below. Hiding an unknown name is an error. | Reading a hidden member from Luau gives the "not a valid member" error. |
| Name default | `Name` defaults to the concrete class name; resolution writes it into each class's default table. Removes the special case in `default_value`. | `default_value_ref("Part", "Name") == "Part"`. |
| Unknown superclass, cycle, more than 65,536 members | Resolution error, typed (`SchemaError`), never a panic. | Kept from today's `replication.rs` tests. |

`IsA` follows only the real superclass chain.

### 3.4 Hierarchy changes

Decision: **insert abstract classes between existing pairs only; never move an existing class**. Reason: every
existing `is_subclass(a, b)` answer stays the same (#460 tests every pair), so no script changes behaviour.

| New abstract class | Above | Carries |
|---|---|---|
| `PVInstance` | `BasePart`, `Model` | `GetPivot`, `PivotTo` (and the faked `IsA` goes away) |
| `GuiBase2d` > `GuiObject` | `Frame` (and through it `TextLabel`, `GuiButton`) | GUI pointer events, shared GUI properties |
| `LuaSourceContainer` > `BaseScript` | `LuaSourceContainer` above `ModuleScript` and `BaseScript`; `BaseScript` above `Script` (and `LocalScript`) | `Source`, `Enabled` |
| `BaseRemoteEvent` | `RemoteEvent`, `UnreliableRemoteEvent` | `FireServer`, `FireClient`, `FireAllClients`, `OnServerEvent`, `OnClientEvent` |
| `ServiceProvider` > `DataModel` | (not above any instance class) | `GetService`; `game` stays the host object for now (section 17) |

`TextLabel` and `GuiButton` stay under `Frame`, which differs from Roblox. Moving them would change `IsA("Frame")`;
that is an owner question, not part of this epic.

Recommended for #466/#467: `UserInputService`, `ContextActionService` and `CollectionService` become `SERVICE` classes
with `Host` members, so they appear in the generated outputs and other languages.

### 3.5 Shared member sets

Forge has "components": named property bundles several classes include. The issue asks whether we need them for
methods and signals too, or whether `PVInstance` is enough.

Decision: **no member sets now; use real abstract classes.** Reasons:

- Everything that shares API today has a natural common superclass in the Roblox hierarchy (`PVInstance`,
  `GuiObject`, `BaseScript`, `BaseRemoteEvent`). Roblox knowledge carries over, and `IsA` stays truthful.
- Mixins inserted at different positions in different classes would break the prefix rule (section 4.1).
- Behaviour (Forge's render and physics hooks) is out of scope, and it was the main reason Forge needed components.

Room left: if a member set is ever needed (for example a transform API for lights and attachments that do not share a
superclass), it is expanded at resolution time into each including class's *own* declared list, as if the class had
declared those members. Ids stay prefix-stable, `IsA` ignores sets, and nothing else in this design changes.

## 4. Resolution and speed

### 4.1 Resolved registry and ids

The registry is built once (a `LazyLock`, as `DEFAULT_VALUES` is today) and then immutable for the process.

```rust
// target API, illustrative
pub struct ClassId(pub u16);            // index into Registry::classes
pub struct MemberId(pub u16);           // class-relative index into ResolvedClass::members
pub struct PropertyIndex(pub u16);      // class-relative, properties only: the wire and storage index

pub struct ResolvedClass {
    pub id: ClassId,
    pub desc: &'static ClassDesc,
    pub chain: &'static [ClassId],      // self first, then superclasses
    pub members: Vec<Member>,           // flattened, base first
    pub defaults: Vec<Value>,           // one per property, Name already set to the class name
    by_name: NameTable,                 // name or alias -> MemberId
}

pub enum Member {
    Property { desc: &'static PropDesc, index: PropertyIndex, owner: ClassId, hidden: bool },
    Method   { desc: &'static MethodDesc, owner: ClassId, decl: DeclId },
    Signal   { desc: &'static SignalDesc, owner: ClassId, decl: DeclId },
    Callback { desc: &'static CallbackDesc, owner: ClassId, decl: DeclId },
}
```

**Prefix rule.** `members` is built root first: `Instance`'s members, then each subclass's own declarations appended.
An override replaces its entry in place; `hide` only sets a flag. So an inherited member has the same `MemberId` (and a
property the same `PropertyIndex`) in the declaring class and in every subclass. Generated wrappers can address
`BasePart.Size` with one id for every kind of part. A test checks the rule for every class pair.

`PropertyIndex` is exactly today's `property_index` (base-first, properties only), so `property_index("Part", "Name")`
stays `Some(0)` and the replication tests keep passing. `DeclId(u32)` is a dense global index over all declarations;
bindings use it to cache one function object per declaration.

World nodes store `ClassId` instead of `&'static str` (`instance.rs:134`). `World::get(id, "Size")` then becomes:
node -> `ClassId` -> resolved class -> name table -> `PropertyIndex` -> stored value or `defaults[index]`. Reason: the
class-name lookup and the chain walk leave the hot path entirely. `World::class_name(id)` returns the registry name.

`class_def`, `find_property`, `is_subclass`, `all_properties`, `class_index`/`class_at`, `property_index`/
`property_by_index` and `default_value(_ref)` stay as thin wrappers so callers migrate later.

### 4.2 Lookup without allocation

- Member keys arrive as borrowed strings (`mlua::BorrowedStr` since #119); the lookup takes `&str` and returns a
  `MemberId`. No allocation on a hit or a miss.
- The name table per class is built once. #460 measures two candidates behind `ResolvedClass::member(name)`: a linear
  scan of the flattened list, and a hash map with a fast non-cryptographic hasher (for example `rustc-hash`, MIT/
  Apache-2.0). It keeps the faster under the perf gate. If they tie, the hash map, because large classes (Lighting has
  22 own properties, GUI classes will have more) cost a scan more. (Estimate: no numbers yet.)
- `is_a` with a `ClassId` is a scan of `chain` (at most a handful of entries). Name-to-`ClassId` (only `Instance.new`,
  `IsA(name)`, scene load) is a per-registry hash map.
- The Luau binding caches one function per method `DeclId` per VM (section 5), so `obj:Method()` creates nothing per
  call. Trying Luau `__namecall` through mlua is part of #462; the result is recorded either way.

### 4.3 Class order, wire indices, registry_hash

Decision: **`ClassId` order is the classes sorted by name.** Reasons: deterministic; no shared list to edit or
conflict on when sessions add classes in parallel; nothing persists a `ClassId` (scenes save names, WASM scripts link
by name, section 13.3), and wire indices are only meaningful between peers with an equal `registry_hash`.

Cost: the 42 current indices change once in #460, and the `registry_hash_is_pinned` golden changes (it changes with
every class change already). Nothing outside `classes.rs` and its tests calls `class_index`/`class_at` on main today.
The "hot classes first" order of `class_slot` is no longer needed once nodes store `ClassId`.

`registry_hash` v5 (`metallic-registry-v5\0`) keeps everything v4 hashes and adds:

| Added to the hash | Not hashed |
|---|---|
| Class flags, `deprecated_by` | Descriptions, categories |
| Every member in `MemberId` order: kind, name, aliases, owner class | Editor metadata (group, order, label, widget) |
| Property `readonly`, `access` kind (stored/computed, settable), `saved`, `enum_type` | Function addresses (as v4 already excludes normalizer addresses) |
| Method params (name, type, optional), variadic, return type, `yields`, implementation kind | Luau snippet source (its declared signature is hashed) |
| Signal and callback params and returns | |
| Member and class context, deprecation | |
| Enums: name, items, numbers, aliases | |

Reason: the hash is a compatibility fingerprint. Two peers that disagree on a signature or a context must refuse each
other; a changed tooltip must not.

### 4.4 Perf gate

Nothing in this document is measured. Today's evidence: the #119 probe measured about 1.6 to 1.9 microseconds p50 per
Luau scalar property read on a busy desktop, with about one allocation request per read left after the key fix
(`docs/research/2026-10-30-luau-property-reads.md`). The gate is the CI instruction count in `docs/performance-ci.md`
(fail above +5% median `Ir` for `reads`, `writes`, `clone`, `create`, `destroy`).

| Step | Gate |
|---|---|
| #460 (registry, `ClassId` in nodes) | Core gate as is: `reads` and `writes` exercise `World::get`/`set` on 1,000 Parts. Expected neutral or better (Estimate). |
| Before #462 | Add Luau workloads (`lua_reads`, `lua_writes`, `lua_method_calls`) in their own PR, so they compile against the base of #462 (the gate requires that). Recommended: a `metallic_script` example driven by the same `measure.py`. |
| #462 onward | Core and Luau workloads. The PR gives numbers. |
| #472 | The vector-math paths keep their native userdata; benchmarks in the PR. |

## 5. Calling convention

### 5.1 ScriptValue

`metallic_core` gets `ScriptValue`, the value that crosses the host interface. No Luau or mlua types in it.

```rust
// target API, illustrative
pub enum ScriptValue {
    Nil,
    Bool(bool),
    Int(i64),                    // Luau numbers that are whole; typed languages' integers
    Number(f64),
    String(Box<str>),
    Instance(InstanceId),        // a handle; liveness checked by the host
    Vector3(Vec3),
    CFrame(Box<CFrame>),
    Color3(Color3),
    UDim2(UDim2),
    EnumItem(EnumItemRef),       // the enum's and the item's static names
    Array(Vec<ScriptValue>),
    Map(Vec<(ScriptValue, ScriptValue)>),
    Function(FunctionRef),       // a binding-owned reference to script code (callbacks, signal handlers)
    Tuple(Vec<ScriptValue>),     // several results
}
```

Core `Value` (what properties store) converts into `ScriptValue` losslessly; the reverse conversion is checked against
the property's `ty`. `Int` is accepted wherever `Number` is declared; a Luau number is accepted as `Int` only when it is
whole and in range.

### 5.2 MethodCall

```rust
// target API, illustrative
pub struct MethodCall<'a> {
    pub world: &'a mut World,
    pub this: InstanceId,
    pub args: &'a [ScriptValue],     // borrowed from a per-VM buffer: no Vec per call
    pub context: &'a CallContext,
    pub(crate) signals: &'a mut SignalQueue,
}

pub struct CallContext {
    pub side: Side,                  // Server or Client (later Studio, Plugin)
    pub script: Option<InstanceId>,  // the calling script, for errors and deprecation warnings
    pub local_player: Option<InstanceId>,
}

pub enum CallOutcome { Return(ScriptValue), Wait(WaitRequest) }
pub type MethodFn = fn(&mut MethodCall) -> Result<CallOutcome, ApiError>;
```

`MethodCall` has typed accessors (`number(i)`, `opt_number(i)`, `instance(i)`, `cframe(i)`, `enum_item(i)`, ...). They
cannot fail on type for `Auto` parameters, because the binding already checked the arguments against the declared
signature; they still return `Result` so a method never panics. `Ok(ScriptValue::Nil.into())` returns nothing.

### 5.3 ApiError

```rust
// target API, illustrative
pub struct ApiError { pub class: &'static str, pub member: &'static str, pub kind: ApiErrorKind }

pub enum ApiErrorKind {
    BadArgument { index: u8, expected: Ty, got: TypeName },
    MissingArgument { index: u8, expected: Ty },
    NotAMember,                         // "<Name> is not a valid member of <Class>"
    ReadOnly,
    CallbackRead,
    WrongContext { required: Side, member_kind: MemberKind },
    NotCreatable,
    Destroyed { name: String },         // "<Name> has been destroyed"
    Core(CoreError),
    Message(Cow<'static, str>),         // method-specific text
}
```

One formatter in `metallic_core` turns an `ApiError` into text, so every language prints the same message. The new
format for argument errors is `Part:Resize: argument #1 expected Enum.NormalId, got string`. Luau keeps wrapping it in
`<Instance full name>:<line>: <message>` (#170).

### 5.4 Yielding

A yielding method returns `CallOutcome::Wait(WaitRequest)`:

```rust
pub enum WaitRequest {
    Child { parent: InstanceId, name: Box<str>, timeout: Option<f64> },
    Signal { key: SignalKey },
    Reply { request: u64 },     // RemoteFunction:InvokeServer and friends
    Seconds(f64),
}
```

The host parks the calling thread, and resumes it with a `ScriptValue` when the request is satisfied (or `Nil` on
timeout). In Luau the generic binding wraps every `yields` method in one shared trampoline written in Luau, because
only Luau code can yield its own coroutine (today's `WaitForChild` already works this way, `instance.rs:554-569`). In
WASM languages the call returns `pending(token)` and the guest awaits (section 13.8). Reason: yielding is part of the
language-neutral API, so `WaitForChild` does not need to be Luau code.

### 5.5 Argument decoding and existing error texts

The binding decodes arguments from the signature: count (missing non-optional, too many when not variadic), then each
type, left to right. The method body sees only valid arguments.

Existing tests assert today's texts, for example `PivotTo: expected a CFrame` (`tests/pivot.rs:358`), `Missing is not
a valid member of Model` (`tests/api.rs:47`, with `ends_with`), `FireServer can only be called from the client`
(`tests/remote.rs:205`). Decision: **migrated members keep their current text.** A parameter whose current error text a
test asserts is declared with its real type (for the docs and types) and `check: Manual`; the method keeps its own
check and message. The "not a valid member" and context messages keep their current wording in the formatter. A later
PR switches to the new format and updates those tests in one place, with the owner's OK (section 17).

### 5.6 Host-implemented methods

Some methods need the script host: they fire remotes, park threads, call back into script code, or read host state
(`Players:GetPlayers` reads the joined set). They are declared in core with `implementation: Host` and implemented in
`metallic_script` with the same neutral signature:

```rust
// in metallic_script, illustrative
pub type HostMethodFn = fn(&mut HostCall) -> Result<CallOutcome, ApiError>;   // HostCall = MethodCall + host state
registry.implement_host("RemoteEvent", "FireServer", fire_server)?;           // resolved to a DeclId once
```

A test walks the registry and fails when a `Host` method has no implementation, or an implementation names no declared
`Host` method. A WASM binding calls the same `HostMethodFn`, so a C# script firing a remote goes through the same code.

## 6. Signals

- Declared with `SignalDesc` on the class that owns them, inherited like any member. #463 declares
  `Instance.ChildAdded/ChildRemoved/Destroying/AncestryChanged/AttributeChanged/Changed`, `BasePart.Touched/TouchEnded`,
  `Humanoid.Died`, `Player.CharacterAdded`, `Players.PlayerAdded/PlayerRemoving`, `RunService.Heartbeat`,
  `BaseRemoteEvent.OnServerEvent/OnClientEvent`, `InputAction.Pressed/Released/Triggered/StateChanged`, and the GUI
  events on `GuiObject`/`GuiButton`.
- Key: `SignalKey { instance: InstanceId, signal: MemberId, arg: u32 }`. `arg` is 0, or an interned name id for
  parameterized signals: `GetAttributeChangedSignal(name)`, `GetPropertyChangedSignal(name)`,
  `CollectionService:GetInstanceAddedSignal(tag)`. This replaces `EventKind::AttributeChanged(u32)`, `TagAdded(u32)` and
  `TagRemoved(u32)` with one rule.
- Firing: core code calls `world.fire(id, signal, args)` (or `MethodCall::fire`). It queues `(SignalKey, args)` in a
  core `SignalQueue`, only when the key has a subscriber (a watched set, so unwatched signals cost one lookup).
  `ScriptHost::step` drains the queue and delivers to each VM's hub: deferred, as today. `ScriptHost::touched` and the
  other entry points stay as the public API for physics and the runtime and enqueue the generic signal.
- Subscribers are language-neutral `FunctionRef`s; `connect_signal` returns a `ConnectionId`. The existing hub rules
  stay: snapshot before dispatch, connection cap, shared step budget, disconnect after repeated timeouts
  (`api/signals.rs:1-10`).
- `Changed` and `GetPropertyChangedSignal` are generic over every property of the class, fed from the existing change
  journal rather than a queue entry per write (coordinate with #410 and #437; `Once`/`Wait` from #404 then work on every
  signal).
- The signal's `context` is checked on connect (for example `OnServerEvent` only on the server).

## 7. Computed properties

A computed property has a getter and an optional setter in its class file:

```rust
// target API, illustrative
pub type Getter = fn(&PropCall) -> Result<Value, ApiError>;
pub type Setter = fn(&mut PropCall, Value) -> Result<(), ApiError>;   // PropCall: world, this, context
```

| Today (special case) | Becomes |
|---|---|
| `ClassName` (`instance.rs:315`, `:380`) | `Instance.ClassName`, computed, read-only |
| `Parent` (`:316-319`, `:381`, `set_parent`) | `Instance.Parent`, computed, `Ref`, setter keeps the "locked" rule for builtins |
| `CFrame` on BasePart (`:320-323`, `:382-391`) | `BasePart.CFrame`, computed from `Position`/`Orientation` |
| `WorldPivot` on Model (`:324-327`, `:392-401`) | `Model.WorldPivot`, computed |
| `LocalPlayer` on Players (`:328-334`) | `Players.LocalPlayer`, computed from `CallContext::local_player` (nil on the server, as today) |
| `Size` clamp (`:411-417`) | `script_write` filter on `BasePart.Size` (applied by bindings, not by scene load) |

Rules: computed properties are `saved: false` and not replicated by themselves (the stored properties behind them
are). The value a setter receives has passed the same type, finite and range checks as a stored write. Computed
properties appear in Studio and the docs like any other.

## 8. Execution context

Each member has a `MemberContext`: `Any`, `Server`, `Client`; later `Studio` and `Plugin` for editor scripts. A class
may set a default for its own members.

- **Enforced in the host interface**, before the getter, setter, method or connect runs, for every language. Methods
  stop checking their VM role (`remote.rs:233, 246, 273`, `input.rs:554-560, 574-588`).
- Declarations from today's checks: `FireServer` Client; `FireClient`, `FireAllClients` Server; `OnServerEvent`
  Server; `OnClientEvent` Client; `InputAction.GetState`, `GetHoldProgress`, `Pressed`, `Released`, `Triggered`,
  `StateChanged` Client; `ContextActionService` members Client.
- Messages: kept exactly as today during the migration. The formatter has the three current shapes: `<Member> can only
  be called from the <server|client>` (methods), `<Member> can only be used from the <server|client>` (signals), and a
  class-scoped `<Class> can only be used from a LocalScript` (selected by a class-level `context_message`, used by
  `InputAction` and `ContextActionService`). #468's single message is the target once the owner agrees (section 17).
- `deprecated` members and classes warn once per script and name the replacement.
- Context is hashed (section 4.3): a server and a client that disagree on it do not connect.

## 9. Enums and data types

**Enums** (#469): `EnumDesc { name, items: &[&str], numbers: Option<&[i64]>, aliases: &[(old, item)], description }`,
declared in the class file that owns them (`Material` and `PartType` in `base_part.rs`, `InputActionType` in `input_action.rs`). Enums used by
several classes live in `classes/enums/<name>.rs`. The `Enum` global, Studio dropdowns, `.d.luau` and WIT enums are
built from them.

- Storage does not change: an enum property stays a `String` value holding the item name, so scene files and
  `registry_hash` meaning stay the same. `allowed` is derived from the enum's items.
- Writes accept the `EnumItem` or its name (today's scripts keep working). Reads return the `EnumItem`: the one behaviour
  change in #469 (section 17, question 2, decided yes). `PropDesc::script_value` and `stored_item` are that conversion at the
  host interface.
- The hand lists in `classes.rs:11-71` and the enum tables built in `api/input.rs` are gone. `MATERIALS` remains as a name
  for `Enum.Material`'s items, because `metallic_render` has one table row per material.

**Data types** (#472): `DataTypeDesc { name, fields, constructors, methods, operators }` with typed signatures, for
`Vector3`, `Vector2`, `CFrame`, `Color3`, `UDim`, `UDim2`, later `TweenInfo` and others. They are value types (copied,
no identity). `api/datatypes.rs` and `api/cframe.rs` keep their native userdata for speed and register against the
descriptors, so the generated outputs and other languages see them. In WIT they are records (section 13.4).

## 10. Luau-implemented members

A class may implement members in Luau in `<class>.luau` next to its class file (`luau: include_str!(..)`), as Forge's
`instance.luau` does. The class file still declares the signature (`implementation: Luau`).

- Snippets run once per VM in a trusted chunk named `=[engine]` and see `methods`, `task`, the signal library and
  `Enum`. Creators cannot reach their locals.
- A test fails when a snippet defines a method the class does not declare, or a declared `Luau` method is missing.
- **Other languages.** A WASM script cannot run a Luau snippet without a Luau VM next to it. Decision: a member any
  other language needs must be native or `Host` (with `CallOutcome::Wait` for yielding), not Luau. The WIT
  generator leaves `Luau` members out and lists them as Luau-only; that list must be empty before 1D. Reason: one implementation per member, no hidden Luau dependency.
- So `WaitForChild` becomes a native yielding method in #465, not a snippet. #470 proves the snippet mechanism with a
  test-only fixture class. The mechanism stays needed for compatibility shims and for creator-defined classes written
  in Luau (section 16.3).

## 11. File layout and how to add a class

```
crates/metallic_core/
  build.rs                      finds `pub static CLASS:` and `pub static ENUMS:` under src/classes/
  src/classes/
    mod.rs                      Registry, lookups, the thin wrappers of today's API
    desc.rs                     ClassDesc, PropDesc, MethodDesc, SignalDesc, CallbackDesc, EnumDesc, Ty
    resolve.rs                  inheritance, overrides, defaults, hide, ids, checks
    call.rs                     ScriptValue, MethodCall, CallOutcome, ApiError and its formatter
    host_api.rs                 the host interface trait (section 13.2)
    prelude.rs                  what a class file imports
    base/       instance.rs, pv_instance.rs, folder.rs, model.rs
    parts/      base_part.rs, part.rs, spawn_location.rs, checkpoint.rs
    services/   data_model.rs, workspace.rs, players.rs, lighting.rs, run_service.rs, replicated_storage.rs, ...
    scripts/    lua_source_container.rs, base_script.rs, script.rs, local_script.rs, module_script.rs
    players/    player.rs, player_gui.rs, player_scripts.rs, humanoid.rs
    gui/        gui_base2d.rs, gui_object.rs, screen_gui.rs, frame.rs, text_label.rs, gui_button.rs, ...
    lighting/   atmosphere.rs, sky.rs, clouds.rs, bloom_effect.rs, color_correction_effect.rs, color_grading_effect.rs
    input/      input_context.rs, input_action.rs, input_binding.rs
    network/    base_remote_event.rs, remote_event.rs, unreliable_remote_event.rs, remote_function.rs
    enums/      enums shared by several classes
```

Decision: **`build.rs` discovery** (as Forge). Reason: adding a class touches no other file, which also removes merge
conflicts between parallel sessions; the alternative (one list checked by a test) puts every new class into one shared
file. The folder is for people; inheritance comes only from `superclass`. `build.rs` reads only file paths and the
marker line; it never parses Rust beyond that. A test asserts every discovered class resolves.

Adding a class: create the file, run `cargo test -p metallic_core` (resolution checks, pinned `registry_hash` tells you
to update the golden), regenerate the outputs (section 12). The class is then in `Instance.new` (if `CREATABLE`),
scene files, Luau, Studio's Insert menu and Properties panel, the reference, `.d.luau`, the JSON dump and WIT.

Complete example. `Beacon` is **not a planned class**; it only shows each feature once.

```rust
//! Beacon: a part that glows and can flash. ILLUSTRATIVE ONLY: the target API of #460-#468.
use crate::classes::prelude::*;

fn glow(get: &PropCall) -> Result<Value, ApiError> {
    let lit = get.bool("Lit")?;
    let range = get.number("Range")?;
    Ok(Value::Number(if lit { range } else { 0.0 }))
}

fn flash(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    let seconds = call.opt_number(0)?.unwrap_or(0.5); // already checked against the signature
    call.world.set(call.this, "Lit", Value::Bool(true))?;
    call.fire(&FLASHED, &[ScriptValue::Number(seconds)])?;
    Ok(ScriptValue::Nil.into())
}

fn beacon_pivot(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    let pivot = crate::transform::get_pivot(call.world, call.this)?;
    Ok(ScriptValue::CFrame(Box::new(pivot)).into())
}

static FLASHED: SignalDesc = SignalDesc::new("Flashed")
    .param("seconds", Ty::Number)
    .describe("Fires after Flash.");

pub static CLASS: ClassDesc = ClassDesc::new("Beacon", Some("Part"))
    .flags(ClassFlags::CREATABLE)
    .category("Effects")
    .describe("A part that glows and can flash.")
    .defaults(&[("Anchored", || Value::Bool(true)), ("CanCollide", || Value::Bool(false))])
    .hide(&["Shape"])
    .props(&[
        PropDesc::number("Range", 16.0)
            .range(0.0, 1000.0)
            .editor(Editor::section(Section::new("Beacon", 50)).order(10).slider(0.0, 200.0))
            .describe("How far the glow reaches, in studs."),
        PropDesc::boolean("Lit", true).describe("Whether the beacon glows."),
        PropDesc::computed("Glow", Ty::Number, glow)
            .describe("Range while lit, else 0."),
    ])
    .methods(&[
        MethodDesc::native("Flash", flash)
            .param_opt("seconds", Ty::Number)
            .context(MemberContext::Server)
            .describe("Lights the beacon and fires Flashed."),
        MethodDesc::host("WaitUntilLit")                 // implemented in metallic_script
            .param_opt("timeout", Ty::Number)
            .returns(Ty::Bool)
            .yields()
            .describe("Waits until Lit is true; false after the timeout."),
        MethodDesc::native("GetPivot", beacon_pivot)
            .returns(Ty::CFrame)
            .overrides()                                 // inherited from PVInstance; same signature
            .describe("The pivot of the beacon."),
    ])
    .signals(&[&FLASHED]);
```

Luau then sees `beacon.Range`, `beacon.Glow` (read-only), `beacon:Flash(1)` (server only), `beacon:WaitUntilLit(5)`,
`beacon.Flashed:Connect(...)`, and every member of `Part`, `BasePart`, `PVInstance` and `Instance`; `beacon.Shape` is
"not a valid member".

## 12. Generated outputs

`cargo run -p metallic_core --example api_dump` (#471) writes, from the resolved registry:

| File | Content | Consumers |
|---|---|---|
| `docs/api/reference.md` | Every class: superclass, flags, description; properties (type, default, context), methods (signature), signals, callbacks; inherited members linked to their declaring class | People; `docs/luau-api.md` keeps its guide parts and links here |
| `docs/api/metallic.d.luau` | `declare class Part extends BasePart ... end`, enums, data types | Studio script editor, luau-lsp |
| `docs/api/api-dump.json` | Language-neutral dump: classes in `ClassId` order, members in `MemberId` order with kinds, signatures, contexts, flags, descriptions, editor metadata; enums; data types; the `registry_hash` | Litha (#141), the WIT and per-language generators (#474) |
| `docs/api/metallic.wit` | Section 13.4 | wasmtime host, guest toolchains |
| `docs/api/bindings/` | Golden typed wrappers for C#, Rust, Python, Java (stubs) | #474 golden tests |

A CI step regenerates and fails when the committed files differ. `.d.luau` excerpt for the example (read-only
property syntax depends on the Luau type solver; (Unverified) which form luau-lsp accepts):

```luau
declare class Beacon extends Part
    Range: number
    Lit: boolean
    Glow: number -- read-only
    Flashed: RBXScriptSignal<number>
    function Flash(self, seconds: number?): ()
    function WaitUntilLit(self, timeout: number?): boolean
end
```

## 13. Every language

C#, Rust, Python and Java (#25, milestone 1D) get the same class-based API with inheritance, generated from the same
registry. Luau is binding #1 of that API, not the API.

### 13.1 Handles

An instance is an opaque `u64` in every language: `InstanceId::to_bits()` (generation in the high half, index in the
low half), which exists today. The handle carries no class and no pointer. The host checks liveness on every use; a
destroyed or stale handle gives `ApiErrorKind::Destroyed` everywhere (consistent with #390). Equality of wrappers is
equality of handles.

### 13.2 Host interface

One interface in `metallic_core`, addressed by ids, used by mlua and wasmtime alike:

```rust
// target API, illustrative
pub trait HostApi {
    fn resolve_class(&self, name: &str) -> Option<ClassId>;
    fn resolve_member(&self, class: ClassId, name: &str) -> Option<MemberId>;
    fn class_of(&self, h: Handle) -> Result<ClassId, ApiError>;
    fn is_a(&self, h: Handle, class: ClassId) -> Result<bool, ApiError>;
    fn get_property(&mut self, h: Handle, class: ClassId, m: MemberId) -> Result<ScriptValue, ApiError>;
    fn set_property(&mut self, h: Handle, class: ClassId, m: MemberId, v: ScriptValue) -> Result<(), ApiError>;
    fn call_method(&mut self, h: Handle, class: ClassId, m: MemberId, args: &[ScriptValue])
        -> Result<CallOutcome, ApiError>;
    fn connect_signal(&mut self, h: Handle, class: ClassId, m: MemberId, arg: u32, f: FunctionRef)
        -> Result<ConnectionId, ApiError>;
    fn disconnect(&mut self, c: ConnectionId);
    fn set_callback(&mut self, h: Handle, class: ClassId, m: MemberId, f: Option<FunctionRef>) -> Result<(), ApiError>;
    fn new_instance(&mut self, class: ClassId, parent: Option<Handle>) -> Result<Handle, ApiError>;
}
```

`class` is the class that declares the member (what a generated wrapper knows). The host checks
`is_a(h, class)`, then indexes the instance's own table with `m` (same entry, by the prefix rule), then the member's
context, then the arguments, then runs it. Every rule lives here once, so no binding can skip it.

Decision: **the trait is defined with #461/#462**, and the Luau binding goes through it from the start (Recommended
over refactoring later in #474). Reason: the existing Luau tests then prove the interface before any WASM language
exists, and #474 becomes generator work. Dynamic languages (Luau) call `resolve_member` per access through the cached
name table; static languages resolve once (13.3).

### 13.3 Linking by name

Ids change whenever a class is added (section 4.3). A compiled C# or Rust script must not embed them. Decision:
**generated wrappers carry `(class name, member name)` pairs and resolve them once when the script module starts**
(`resolve_class`, `resolve_member`), storing the ids in a per-module table. After that every access is by id. A missing
name (a script built for a newer engine) fails at load with a clear error, not at the first call. Reason: version
tolerance at no per-call cost; the same idea as replication comparing `registry_hash` first.

### 13.4 WIT shape

Recommended: **a small generic WIT interface plus generated typed classes per language**, instead of the "one
`resource` per class" in #474's text. Reasons:

- WIT has no inheritance; typed classes with real inheritance must be generated per language anyway.
- About 600 classes as resources would make every class change a change of the component's imported interface; a
  generic interface stays stable, and the typed layer is plain generated source code.
- One generic path means every call goes through the same `HostApi` checks.
- Instance handles are not owned by scripts (the World owns instances), so WIT `resource` ownership and drop semantics
  do not fit; a `u64` handle does.

WIT has no recursive types (Unverified here; check at #474), so nested arrays and maps travel as a flat list of
values with index ranges, and the generated helpers rebuild native collections.

```wit
// illustrative
package metallic:host@0.1.0;

interface types {
    type handle = u64;
    type class-id = u16;
    type member-id = u16;
    record vector3 { x: f64, y: f64, z: f64 }
    record cframe { m: list<f64> }                 // 12 row-major components
    record color3 { r: f64, g: f64, b: f64 }
    record udim2 { x-scale: f64, x-offset: f64, y-scale: f64, y-offset: f64 }
    record enum-item { enum-id: u16, item: u16 }
    record span { start: u32, len: u32 }            // into the same flat value list
    variant value {
        nil, boolean(bool), int(s64), number(f64), text(string), instance(handle),
        vec3(vector3), cf(cframe), color(color3), udim(udim2), enum-value(enum-item),
        array(span), map(span), function(u32),
    }
    record values { items: list<value>, top: span }
    record api-error { kind: u8, class: string, member: string, arg: option<u8>, message: string }
    variant call-outcome { returned(values), pending(u64) }
}

interface instances {
    use types.{handle, class-id, member-id, values, api-error, call-outcome};
    resolve-class: func(name: string) -> option<class-id>;
    resolve-member: func(class: class-id, name: string) -> option<member-id>;
    class-of: func(h: handle) -> result<class-id, api-error>;
    is-a: func(h: handle, class: class-id) -> result<bool, api-error>;
    get-property: func(h: handle, class: class-id, member: member-id) -> result<values, api-error>;
    set-property: func(h: handle, class: class-id, member: member-id, v: values) -> result<_, api-error>;
    call-method: func(h: handle, class: class-id, member: member-id, args: values) -> result<call-outcome, api-error>;
    connect-signal: func(h: handle, class: class-id, member: member-id, arg: u32, callback: u32) -> result<u64, api-error>;
    disconnect: func(connection: u64);
    new-instance: func(class: class-id, parent: option<handle>) -> result<handle, api-error>;
}

world script {
    import instances;
    export invoke: func(callback: u32, args: values) -> result<values, string>;
    export resume: func(token: u64, result: result<values, api-error>);
}
```

Enums are also emitted as WIT `enum`s for the typed layers; the generic interface passes `enum-item`.

### 13.5 Type mapping

| `Ty` / ScriptValue | Luau | WIT | C# | Rust | Python | Java |
|---|---|---|---|---|---|---|
| `Nil` | `nil` | `value::nil`, or `option` none | `null` / `void` | `()` / `None` | `None` | `null` / `void` |
| `Bool` | `boolean` | `bool` | `bool` | `bool` | `bool` | `boolean` |
| `Int` | `number` (whole) | `s64` | `long` | `i64` | `int` | `long` |
| `Number` | `number` | `f64` | `double` | `f64` | `float` | `double` |
| `String` | `string` | `string` | `string` | `String` (in), `&str` accepted | `str` | `String` |
| `Instance(C)` | `Instance` userdata | `handle` (`u64`) | generated class `C` | generated struct `C` | generated class `C` | generated class `C` |
| `Vector3` | `Vector3` userdata | `record vector3` | `readonly struct Vector3` | `struct Vector3` (`Copy`) | frozen dataclass `Vector3` | `record Vector3` |
| `CFrame` | `CFrame` userdata | `record cframe` | `readonly struct CFrame` | `struct CFrame` (`Copy`) | frozen dataclass | `record CFrame` |
| `Color3`, `UDim2` | userdata | records | `readonly struct` | `struct` (`Copy`) | frozen dataclass | `record` |
| `Enum(E)` | `EnumItem` (`Enum.Material.Plastic`) | `enum-item`; WIT `enum` per enum | `enum E` | `enum E` | `enum.Enum` subclass | `enum E` |
| `Array(T)` | table `{T}` | `array(span)` | `IReadOnlyList<T>` | `Vec<T>` | `list[T]` | `List<T>` |
| `Map(T)` | table `{[string]: T}` | `map(span)` (key, value pairs) | `IReadOnlyDictionary<string, T>` | `BTreeMap<String, T>` | `dict[str, T]` | `Map<String, T>` |
| `Optional(T)` | `T?` | `option<T>` / `nil` case | `T?` | `Option<T>` | `T \| None` | `@Nullable T` |
| `Function(sig)` | function | `u32` callback id; guest `invoke` | delegate (`Action<..>`, `Func<..>`) | `Box<dyn FnMut(..)>` | callable | functional interface |
| Signal member | `RBXScriptSignal` | `connect-signal` + callback id | `Signal<T..>` with `Connect(Action<T..>)` | `Signal<T>` with `Connect(closure)` | `Signal` with `Connect(fn)` | `Signal<T>` with `Connect(Consumer<T>)` |
| Connection | `RBXScriptConnection` | `u64` | `Connection` (`IDisposable`) | `Connection` (disconnect on `Disconnect()`, not on drop) | `Connection` | `Connection` (`AutoCloseable`) |
| `Tuple` | several returns | `values` | `ValueTuple` | tuple | tuple | generated `record` |
| `ApiError` | error string with `<script>:<line>:` | `api-error` | `MetallicException` | `Result<T, ApiError>` | `MetallicError` | `MetallicException` (unchecked) |
| Data types of #472 | userdata | records | `readonly struct` | `Copy` struct | frozen dataclass | `record` |

Numbers are f64 everywhere, as in Luau; no language silently gets f32.

### 13.6 Classes and naming per language

Every language gets real classes with the registry's inheritance, generated from `api-dump.json`:
`Part extends BasePart extends PVInstance extends Instance`.

| Language | Inheritance | Property | Method | Signal | Downcast |
|---|---|---|---|---|---|
| Luau | registry chain behind one userdata; `.d.luau` `extends` | `part.Size` | `part:PivotTo(cf)` | `part.Touched:Connect(f)` | `IsA` |
| C# | `class Part : BasePart` | `part.Size` | `part.PivotTo(cf)` | `part.Touched.Connect(f)` | `inst.As<Part>()` (null if not) |
| Rust | `struct Part(BasePart)` with `Deref<Target = BasePart>` down to `Instance` | `part.Size()?`, `part.SetSize(v)?` | `part.PivotTo(cf)?` | `part.Touched().Connect(f)?` | `inst.cast::<Part>()` (`Option`) |
| Python | `class Part(BasePart)` with `property` | `part.Size` | `part.PivotTo(cf)` | `part.Touched.Connect(f)` | `inst.as_(Part)` (None if not) |
| Java | `class Part extends BasePart` | `part.Size()`, `part.SetSize(v)` | `part.PivotTo(cf)` | `part.Touched().Connect(f)` | `inst.as(Part.class)` (null if not) |

Decision: **names stay PascalCase in every language**, and properties become accessor methods (`Size()`/`SetSize(v)`)
only where the language has no properties (Rust, Java). Reason: one name in the docs, Litha and Roblox knowledge,
whatever the language; generated Rust uses `#[allow(non_snake_case)]`. Downcasts call `is_a` on the host, so they are
always truthful. Rust uses `Deref` for inheritance (simple, completes in editors); the trait-per-class alternative is
noted for #474.

### 13.7 Errors

The host formats the message (section 5.3); each language wraps it in its idiomatic error and keeps the structured
fields (`kind`, `class`, `member`, `arg`). So a wrong argument reads the same in every language. An uncaught error ends
that script thread or task, as in Luau today, and is printed with the script name.

### 13.8 Yields and async

Non-yielding members are plain synchronous calls in every language. Yielding members (`yields`, `task.wait`) map:

| Language | Shape | How the host drives it |
|---|---|---|
| Luau | Coroutine yield (`task.wait(1)`, `part:WaitForChild("X")`) | Host resumes the thread (today's scheduler) |
| C# | `await task.Wait(1)`, `await part.WaitForChild("X")` (returns `ValueTask<T>`) | `call-method` returns `pending(token)`; a single-threaded synchronization context in the guest continues on `resume` (Unverified: componentize-dotnet async support) |
| Rust | `async fn` and `.await` | A small single-threaded executor in the guest library, woken by `resume` |
| Python | `async def` and `await` | A guest event loop pumped by `resume` (Unverified: componentize-py async support) |
| Java | `CompletableFuture<T>` with `thenAccept` | Completed by `resume`; blocking style only if the chosen toolchain gets stack switching (Unverified) |

Signal handlers run as their own task in every language, as Luau runs each handler in its own thread; in the async
languages a handler may be `async`. The design does not depend on native Component Model async; `pending` + `resume`
works with a plain synchronous component interface.

### 13.9 Sandbox and context below the language

| Rule | Luau today | WASM (wasmtime) |
|---|---|---|
| Time budget per step | Interrupt against `STEP_BUDGET_SEC` (`vm.rs:19`) | Epoch interruption with the same budget (fuel only if determinism is needed) |
| Memory cap | `set_memory_limit` (`vm.rs:487`) | `StoreLimits` / `ResourceLimiter`, same cap |
| No file, network or OS access | Sandboxed globals | The `script` world imports only `metallic:host`; no WASI filesystem or sockets |
| Execution context | Host interface (section 8) | Same host interface |
| Argument checks, error text | Host interface (section 5) | Same |
| Instance liveness | Host interface | Same |
| Remote size and queue limits | `api/remote.rs` limits | Same `HostMethodFn` |

Each WASM module instance belongs to one `Side`, as each Luau VM does. Nothing a binding does can bypass these, because
they are checked in `HostApi` and in the store configuration, not in generated wrapper code.

## 14. Migration plan

Order follows #458. Every step keeps `crates/metallic_script/tests` (and core, replication and Studio tests) passing
unchanged. **Rule:** from the day #460 lands, new classes and properties go only into class files; from the day #462
lands, new methods, signals and callbacks too. #462 adds a test that `api/instance.rs` contains no class-name string
literals; reviewers reject new `class == "..."` arms.

| Step | Issue | Adds | Removes |
|---|---|---|---|
| 1 | #459 | This document | |
| 2 | #460 | `classes/` directory, `ClassDesc` with flags and member lists, `build.rs` discovery, resolution checks, `ClassId`/`MemberId`/`PropertyIndex`, `ClassId` in World nodes, abstract classes (3.4), `registry_hash` v5, abstract/service refusal in `Instance.new` | `classes.rs` (`CLASSES`, `class_slot`, chain walk in `find_property`), the `Name` default special case |
| 3 | #461 | `ScriptValue`, `MethodCall`, `CallOutcome`, `ApiError` and formatter, `Host` method registration and its completeness test, the `HostApi` trait | |
| 4 | (before #462) | Luau perf workloads (4.4) | |
| 5 | #462 | Generic `__index`/`__newindex` through `HostApi`; per-VM function cache by `DeclId`; `Instance.new` from `CREATABLE` | `CLASS_METHODS`, method allow-list check, `PVInstance` case in `IsA`, `is_service` use in `Instance.new` |
| 6 | #463 | `SignalDesc`, `SignalKey`, core `SignalQueue`, generic `Changed`/`GetPropertyChangedSignal` | `EventKind`, `event_kind`, `GuiEventKind::of` class checks |
| 7 | #464 | Computed properties, `script_write` | `ClassName`/`Parent`/`CFrame`/`WorldPivot`/`LocalPlayer` arms and the `Size` clamp case in `instance.rs` |
| 8 | #465 (parallel) | Instance tree, attribute, tag, `PVInstance`, `Model`, `BasePart` members in class files; native yielding `WaitForChild` | `api/pivot.rs`, `methods()` in `instance.rs`, method tables in `attributes.rs` and `tags.rs` |
| 9 | #466 (parallel) | `DataModel:GetService` from `SERVICE`, `Players`, `Player`, `RunService`, `BaseRemoteEvent`, `RemoteFunction` members | `SERVICES` in `services.rs`, member registration in `remote.rs` (the queue and wire code stays) |
| 10 | #467 (parallel) | `Humanoid`, `InputAction`, GUI members; decision on `UserInputService`/`ContextActionService` (3.4) | `check_input_member`, GUI and input class checks |
| 11 | #468 | Context per member, checked in `HostApi`; deprecation warnings | Role checks in `remote.rs` and `input.rs` |
| 12 | #469 | `EnumDesc`, `Enum` global from the registry | `MATERIALS`, `SHAPES`, `INPUT_*`, `LIGHTING_STYLES`, `TONEMAPPER_PRESETS`, hand-built enum tables in `input.rs` |
| 13 | #470 | `.luau` snippets next to class files (fixture class) | |
| 14 | #471 | `api_dump`, `docs/api/*`, CI freshness check | Hand-maintained member tables in `docs/luau-api.md` (guide text stays) |
| 15 | #472 | `DataTypeDesc` for `Vector3`, `CFrame`, `Color3`, `UDim2`, ... | Nothing; native userdata stays |
| 16 | #473 | Studio Properties panel and Insert menu from the registry | `section` and `SECTION_NAMES` in `properties.rs`, hard-coded Insert lists |
| 17 | #474 | `metallic.wit`, golden C#/Rust/Python/Java wrappers, name linking | Anything Luau-specific left in the host path |

#468 and #469 only need #462 and #460 and may start before the migrations finish. A step that cannot keep a test
unchanged stops and asks (section 17), rather than editing the test.

## 15. Files that go away or shrink

| File | Fate | Step |
|---|---|---|
| `crates/metallic_core/src/classes.rs` | Gone; replaced by `src/classes/` | #460 |
| `crates/metallic_script/src/api/pivot.rs` | Gone; members in `pv_instance.rs`, `model.rs`, `base_part.rs` | #465 |
| `crates/metallic_script/src/api/instance.rs` | Shrinks to the generic binding (userdata, `__index`, `__newindex`, waiters) with no class names | #462-#465 |
| `crates/metallic_script/src/api/signals.rs` | Keeps the hub; `EventKind` goes | #463 |
| `crates/metallic_script/src/api/services.rs` | Keeps `game`; `SERVICES` goes | #466 |
| `crates/metallic_script/src/api/remote.rs` | Keeps the queue and wire limits; member wiring and role checks go | #466, #468 |
| `crates/metallic_script/src/api/input.rs`, `gui.rs` | Keep host-side state and events; member lookup and class checks go | #467 |
| `crates/metallic_studio/src/properties.rs` | `section` and `SECTION_NAMES` go | #473 |

## 16. Alternatives considered

### 16.1 A proc-macro `#[class]` on structs

A derive such as `#[class(superclass = "Part")] struct Beacon { #[prop(default = 16.0)] range: f64 }` would remove some
builder text. Rejected for now: it adds a proc-macro crate and compile time to the most-edited crate, hides the
generated tables from readers and from `build.rs`, gives worse error messages, and suggests Rust struct fields hold the
data when the World does. `const` builders in plain Rust give the same single-file result and are what Forge proved.
Revisit only if the boilerplate becomes a measured problem; the descriptors would stay the same, so a macro could be
added later as sugar.

### 16.2 Exposing ECS components directly

Scripts would see data components (Transform, Appearance, ...) instead of classes. Rejected: Metallic has its own typed
instance World, not an ECS (engine crates such as bevy are not allowed); exposing storage freezes its layout and blocks
#187's compact storage; and the Roblox-style API creators know is classes with inheritance. Forge also kept components
internal and gave scripts classes.

### 16.3 Runtime-registered creator-defined classes

A package defines `Coin` inheriting from `Part`, in any language, with its own properties, methods and signals. Not
built in this epic; the design leaves room:

- The resolved registry uses indices, not addresses, so a second "package" layer of classes can be appended after the
  built-ins at game load. Built-in ids do not move; creator ids follow.
- Descriptors would gain an owned form (`ClassSpec`, loaded from the package's manifest) next to the `&'static` one;
  resolution, the rules of section 3.3 and the host interface are the same. Built-in classes stay `&'static`.
- Members get a fourth implementation kind, `Script(FunctionRef)`, called through the same `HostApi`, in the
  creator's language. Luau snippets (section 10) are the first instance of this.
- `registry_hash` covers the creator layer too, so peers of one game agree on it.
- Creator classes cannot override built-in members marked `final` (a flag added then), and cannot change built-in
  contexts.

## 17. Open questions for the owner

1. **Unify error texts after the migration?** One PR would switch to `Class:Member: argument #n expected T, got U`,
   `<Member> can only be used from a <Server|Client> script` and `X is not a valid member of <Class> "<full name>"`,
   and update the tests that assert today's texts. Recommended: yes, right after #468.
2. **Enum reads return `EnumItem` (#469)?** Roblox-compatible, but `print(part.Material)` then prints
   `Enum.Material.Plastic`, and `part.Material == "Plastic"` becomes false. Writes accept both either way.
   Decided: yes, as its own PR (#469). `docs/luau-api.md` "Enums" lists what scripts notice.
3. **WIT shape:** generic id-based interface plus generated typed classes (Recommended, 13.4), or one WIT resource per
   class as #474's text says?
4. **`game` as a real `DataModel` instance** with services as its children (`workspace.Parent == game`)? Changes
   tree shape and replication. Recommended: later, with the replicator.
5. **GUI hierarchy:** keep `TextLabel` and `GuiButton` under `Frame` (Recommended; no `IsA` change), or move them to
   `GuiObject` as in Roblox?
