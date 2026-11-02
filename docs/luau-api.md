# Luau API (Metallic Engine, milestone 1)

Scripts are sandboxed Luau with Roblox-style names. `Script`s run in the **server VM**, `LocalScript`s in the **client VM**
(one local player, `Player1`, in the default combined host). Errors read `<Instance full name>:<line>: <message>` and go to the output sink; one failing
script or handler never stops the others. Every call into a script (start, handler, Heartbeat, remote) has a 0.1 s budget.

Code: `crates/metallic_script/src/api/`. Tests: `crates/metallic_script/tests/{api,remote,cframe,pivot}.rs`.

## Where scripts run
In combined mode, `ScriptHost::start` runs in tree order: every enabled `Script` under Workspace or ServerScriptService (server VM) and every
enabled `LocalScript` under StarterPlayerScripts or a `PlayerGui` (client VM). Scripts added later to those places (for example
StarterGui contents cloned into the `PlayerGui`) start on the next `step`. `script` is the script's own Instance.
The runtime (`Game::start`) copies the children of `StarterGui` into `Player1.PlayerGui` once when the game starts (M1 has
no `ResetOnSpawn`: the GUI survives respawns).

`ScriptHost::new_server(world, output)` selects a fixed server-only host. It starts only the server VM and eligible
`Script`s; `LocalScript`s stay inactive, including those created, enabled or reparented while running. It creates no
synthetic local Player, PlayerGui or PlayerAdded event, and preserves existing world instances. `Players.LocalPlayer`
is nil; no player is admitted by this constructor. Server events, Heartbeat, waits and script-owner cleanup use the
existing host paths. Client-targeted RemoteEvents have no receiver and are discarded at the next delivery step.
Host-injected client action events likewise have no receiver and are discarded on the next step; InputAction state
access keeps its existing client-only restriction.
The default `ScriptHost::new` and the current Game/Studio combined mode keep their existing behavior.

`ScriptHost::with_role(world, output, ScriptRole::{Combined, Server, Client})` selects a
fixed VM role; `role()` reports it. A Client host starts one client VM and creates no
Player, GUI or Character. Before `set_local_player(player)`, no LocalScript runs.
Binding accepts only a live Player directly under this World's built-in Players service.
After binding, the next step runs enabled LocalScripts under the existing
StarterPlayerScripts path, the local Player's PlayerGui, or its current Character.
Another Player's private GUI and server Scripts do not run in this Client VM.
Rebinding cancels old client-owned tasks/connections and reruns eligible scripts in the
new context. Pending action and GUI input from the old binding is discarded; assigning
the same valid binding is a no-op and retains its queued input. Local removal or an
invalidated binding also clears pending GUI input. Character-reference or ownership changes recheck eligibility without
restarting unrelated starter/GUI scripts. This is execution policy over a trusted World,
not authentication or replication filtering.
Client input/lifecycle dispatch also prunes invalid owners before executing handlers.
This prune does not start new scripts or consume the journal changes needed by step;
input execution waits when a World borrow prevents validating the current scope.
Client step likewise postpones all Lua work until the World is readable; queued
valid work and held frame deadlines remain intact. Server/Combined stepping keeps
its existing behavior.

The host's trusted player lifecycle APIs do not create or destroy World instances:

- `add_player(player)` registers an existing world-local Player and queues PlayerAdded.
  Duplicate registration returns false. GetPlayers includes the player when Added fires.
- `remove_player(player)` returns false after the first removal. In a running session,
  a pending Added fires before Removing, and the player is still in GetPlayers during
  immediate Removing dispatch. The caller destroys the Player and Character after removal and before the
  next step; normal journal synchronization then cancels destroyed scripts' owned work.
  Stale registered handles are forgotten without callbacks; unknown handles are no-ops.
- `set_local_player(player)` is Client-only and separate from roster registration.
  Stale/non-Player/misparented bindings or additions error without replacing valid state.
  A busy World rejects lifecycle mutations without partially changing them.
- `stop` drops VM/session work, retaining the World and registrations. Restart validates
  live handles and replays Added once in World child order. Stale bindings clear, and
  recreated same-name players require explicit registration/binding. A removed local
  Client remains running but unbound until a later valid binding.

Lifecycle events use the existing signal/resume budgets. A callback may yield or be
deferred, so removal does not promise that every handler finishes before destruction.
GetPlayers reads the current roster; a deferred callback may see the player already removed.
PlayerScripts class-dependent coverage remains separate while its core registration
is pending; this layer does not fabricate a replacement class or clone player containers.

These roles are a scripting foundation only. They add no network transport, player admission,
dedicated-server process or playable multiplayer. The ModuleScript loader below is retained. Existing
sandbox/scheduler behavior is retained; process/transport/isolation work remains separate.
M2 epic #15 acceptance is unchanged.

## Globals
`game`, `workspace`, `script`, `print`, `warn`, `task` (`wait`, `spawn`, `delay`), `require`, `Instance`, `Vector3`, `Vector2`, `CFrame`, `Color3`, `UDim2`, `Ray`, `Enum`.

## ModuleScript and `require`
`ModuleScript` (property `Source`) never runs by itself. `require(moduleScript)` runs its `Source` once per script VM, as ordinary
sandboxed Luau under the requiring thread's deadline (it may `task.wait`), and returns the module's single return value to every
caller; `script` inside the module is the ModuleScript. The cache is scoped to the VM, so the server VM and the client VM of a
host each run a module once. Explicit Server and Client hosts use the same loader rules. A Client rebind retains completed
module values in its existing VM, while stopping that Client script releases any unfinished load it owns. Restarting the
host creates a fresh cache. Rules, as in Roblox:

- the module must return exactly one value (`nil` counts): otherwise `<Full.Name>: Module code did not return exactly one value`;
- a cyclic require errors with `Requested module was required recursively` instead of hanging;
- an error in the module (including a syntax error) propagates to the requiring script with the module's own position, e.g.
  `ReplicatedStorage.Config:3: boom`, and is cached: later requires raise the same error without running the module again;
- only a `ModuleScript` instance is accepted: anything else raises `Attempted to call require with invalid argument(s).`;
- the cached value is kept for the life of the VM; changing `Source` afterwards has no effect until the VM restarts.

A second script that requires a module while another thread is still loading it (the module yielded, for example in
`task.wait`) **waits** for it and gets the same value; it is a cycle only when the requiring thread is the loading thread or
the loader is itself waiting on a module the requiring thread is loading. A script that is stopped while loading a module
releases it, so the next `require` loads it afresh. If the thread loading a module dies without reporting (a timeout outside
the module's own error handling), the module is marked failed with `<Full.Name>: Requested module experienced an error while
loading`, and every later `require` raises that. A client VM cannot require a ModuleScript under `ServerScriptService` or
`ServerStorage`: it raises `Attempted to require a ModuleScript that is only available on the server`.

## Instance
- `Instance.new(class, parent?)`. Services and abstract classes (`BasePart`, `PVInstance`, ...) cannot be created; an unknown class errors.
- Members: `Name`, `Parent`, `ClassName` (read-only), `FindFirstChild(name, recursive?)`, `WaitForChild(name, timeout?)`,
  `GetChildren()`, `GetDescendants()`, `IsA(class)`, `Destroy()`, `Clone()`, `Changed`, `GetPropertyChangedSignal(name)` (see "Property changes"), and every registry property of the class by name.
- `instance.ChildName` finds a child. Unknown member: `<Name> is not a valid member of <ClassName>`.
- Member keys must be strings; numeric keys are rejected. Invalid UTF-8 bytes in a string key keep the existing
  replacement-character behavior for lookup and errors.
- Wrong property type: `invalid value for property Position: expected Vector3, got number`. Numbers and vectors must be finite.
- The same instance is always the same Lua object (`a == b`, usable as a table key).
- A destroyed instance errors on use: `Brick has been destroyed`. Services cannot be destroyed, cloned or reparented.
- `WaitForChild` yields; `step` resumes it. With a timeout it returns nil afterwards. Without one it warns once after 5 s.

```lua
local part = Instance.new("Part", workspace)
part.Position = Vector3.new(0, 10, 0)
print(workspace:WaitForChild("Spawn", 5), #workspace:GetChildren())
```

## Attributes
Any instance (parts, models, services, scripts) can carry named values outside its class properties.

- `inst:SetAttribute(name, value)`; a `nil` value removes it. `inst:GetAttribute(name)` is `nil` when unset.
  `inst:GetAttributes()` returns a new table `name -> value`.
- Value types: boolean, number, string, `Vector3`, `Color3`, `UDim2`, `CFrame`, `nil`. Numbers must be finite; a string is at
  most 65536 bytes; one instance holds at most 1024 attributes.
- Names: 1 to 100 characters, ASCII letters, digits and `_`, and not starting with `RBX` (any case). A bad name or value
  raises an error naming the problem. Setting the value an attribute already has changes and fires nothing.
- `inst.AttributeChanged:Connect(function(name) ... end)` and `inst:GetAttributeChangedSignal(name):Connect(function() ... end)`
  fire for a set, change or removal. Like the other signals they run in the next `step`, whoever made the change (this VM, the
  other VM or the editor); the handler reads the current value with `GetAttribute`, so two changes in one step fire twice and
  both see the last value. `GetAttributeChangedSignal(name)` checks the name like `SetAttribute` does, and a VM can make
  signals for at most 4096 different attribute names. No event is dropped silently: the world keeps every change until each
  VM has read it (hundreds of thousands fit), and when a step runs out of time the rest of the events wait in that
  log and are delivered in the next steps, in order (the handler queue is not used for them). The log holds 262,144 unread
  changes; one more drops the oldest half (about 131,000), and the VM then warns `attribute change events were lost` and
  continues with the rest, so only a backlog that large loses events. Nothing is dropped below that.
- Attribute data is outside the VM's memory cap, so a world holds at most 16 MiB of it (names, values and, with tags,
  tags); beyond that `SetAttribute` and `Clone()` raise a budget error until something is removed or destroyed.
- `Clone()` copies attributes; scene files store them (`attributes: [("Name", Value), ...]` per instance, written only when
  there are any, so older files and older Studios keep working); Studio shows them under **Attributes** in Properties (add,
  edit Bool/Number/String, remove; every change is one undo step).
- Core: `World::set_attribute`, `attribute`, `attributes`, and the bounded attribute log (`attribute_cursor`,
  `attribute_changes_since`) that readers such as the script VMs and, later, replication (#305) follow.

## Tags and CollectionService
Tags are text labels on instances (Roblox's `CollectionService`). `game:GetService("CollectionService")` (or
`game.CollectionService`):

- `CS:AddTag(instance, tag)`, `CS:RemoveTag(instance, tag)`, `CS:HasTag(instance, tag)`, `CS:GetTags(instance)` (sorted array),
  and the same as instance methods: `inst:AddTag(tag)`, `inst:RemoveTag(tag)`, `inst:HasTag(tag)`, `inst:GetTags()`.
- `CS:GetTagged(tag)` returns the instances that carry the tag **and are in the game** (they descend from a service; a
  detached instance, a fresh `Clone()` or an instance with no parent is not returned until it is parented). It looks the tag up
  in an index, so its cost follows the number of tagged instances, not the size of the tree. Order: by instance id. Destroyed
  instances are gone from every set. `CS:GetAllTags()` lists every tag some instance carries (sorted).
- `CS:GetInstanceAddedSignal(tag)` and `CS:GetInstanceRemovedSignal(tag)` pass the instance. They fire when the tag is added or
  removed on an instance in the game, when a tagged instance enters the game (parented under a service) or leaves it
  (parented to nil), and, for removed, when a tagged instance is destroyed (the handler gets the destroyed instance). Like the
  other signals they run in the next `step`, whoever made the change; connecting does not replay existing instances, so also
  loop over `GetTagged(tag)`. `GetInstanceAddedSignal` and `GetInstanceRemovedSignal` check the tag like `AddTag` does, and a VM can make signals for at most 4096 different tags. Every change is delivered (destroying thousands of tagged instances fires one removal each; the world keeps changes until every VM has read them, and when a step runs out of time the remaining events wait in that log and are delivered in the next steps, in order: 20,000 tagged instances destroyed at once reach `GetInstanceRemovedSignal` handlers one by one over several steps. The log holds 262,144 unread changes; one more drops the oldest half, the VM warns `tag events were lost` and continues with the rest). Tags share the 16 MiB attribute data budget of a world.
- A tag is 1 to 255 bytes without control characters; an instance holds at most 256 tags. Tagging twice or removing a missing
  tag does nothing. `Clone()` copies tags; scene files store them (`tags: ["A", "B"]` per instance, only when present, so older
  files and older Studios keep working); Studio shows them under **Tags** in Properties (add and remove, one undo step each).
- Core: `World::add_tag`, `remove_tag`, `has_tag`, `tags`, `tagged`, `all_tags`, `is_in_tree`, and the bounded tag log
  (`tag_cursor`, `tag_changes_since`) the script VMs follow. Tagged-subtree bookkeeping on reparent and destroy only runs
  while some tag exists in the world.

## Enums
`Enum.<Name>.<Item>` is built from the class registry; `docs/design/class-system.md` section 9 has the design and
ARCHITECTURE.md "How to add a class" has how to declare one.

An `EnumItem` has `Name` (string), `Value` (number: the item's position from 0 unless the enum gives numbers, as
`Enum.ContextActionPriority` does) and `EnumType` (its enum, so `Enum.Material.Metal.EnumType == Enum.Material`).
`typeof(item)` is `"EnumItem"`; it prints as `Enum.Material.Metal`. Items and enums are read-only and a wrong member errors
(`Nope is not a valid member of Enum.Material`). `Enum.Material` itself is a read-only table: `tostring` gives `Material`,
`typeof` gives `"table"`, and `pairs` lists its items and any old names.

Enum properties read as an `EnumItem` and take the item or its name:

| Class | Property | Enum |
|---|---|---|
| `BasePart` | `Material`, `Shape` | `Enum.Material`, `Enum.PartType` |
| `Camera` | `CameraType` | `Enum.CameraType` |
| `InputAction` | `Type`, `Trigger`, `Role`, `TouchSlot`, `TouchLayout` | `Enum.InputActionType`, `Enum.InputTrigger`, `Enum.InputRole`, `Enum.TouchSlot`, `Enum.TouchLayout` |
| `Lighting` | `LightingStyle` | `Enum.LightingStyle` |
| `ColorGradingEffect` | `TonemapperPreset` | `Enum.TonemapperPreset` |

```lua
part.Material = Enum.Material.Metal
part.Material = "Metal"                  -- the same; scene files store this name
print(part.Material == Enum.Material.Metal)  --> true
print(part.Material.Name)                --> Metal
print(part.Material)                     --> Enum.Material.Metal
```

A name that is no item, or an item of another enum, is an error and changes nothing:
``invalid value for `Material`: `Lava` is not one of [...]``, `invalid value for property Material: expected Enum.Material,
got Enum.PartType.Ball`.

Since #469 these properties read as an item, not a string. A script that held a string changes like this:

| The script does | Before | Now | Write instead |
|---|---|---|---|
| `part.Material == "Metal"` | `true` | `false`, with no error (Luau never calls `__eq` across types; Roblox behaves the same) | `part.Material == Enum.Material.Metal` or `part.Material.Name == "Metal"` |
| `print(part.Material)`, `tostring(part.Material)` | `Metal` | `Enum.Material.Metal` | `part.Material.Name` |
| `"x" .. part.Material`, `string.lower(part.Material)` | works | error | `part.Material.Name` |
| `other.Name = part.Material`, any String property or argument | works | error | `part.Material.Name` |
| `other.Material = part.Material`, a remote argument | works | works: a remote carries the item | |
| `part:SetAttribute("M", part.Material)` | stores the name | error: attributes hold no enum item | `part.Material.Name` |
| `t[part.Material]` | key `"Metal"` | key is the item | `t[part.Material.Name]` |

`InputBinding.KeyCode` and the other binding fields are not enum properties: they name runtime controls (`"None"` is not an
`Enum.KeyCode` item), read as strings and still take `Enum.KeyCode.X` when written. `LookOverride` also stays a string.

## Camera
`workspace.CurrentCamera` is a `Camera` instance under the Workspace (nil in a bare script host; `Game` creates it when it
starts). Properties: `CFrame`, `FieldOfView` (vertical, degrees, 1 to 120, default 70), `CameraType`
(`Enum.CameraType.Custom`, `Scriptable` or `Fixed`; see [Enums](#enums)) and `ViewportSize` (a read-only
`Vector2`, the size of the viewport the game is drawn in; 1280 x 720 until the host reports one).

- **Custom** (the default): the game's third-person camera. Every frame it publishes its pose to `CFrame` and `FieldOfView`, so
  `workspace.CurrentCamera.CFrame` is where the camera is; writes are overwritten the next frame.
- **Scriptable** (and **Fixed**, the same here): the script's `CFrame` and `FieldOfView` are the camera. Set `CameraType` back
  to `Custom` and the game's camera takes over again. The drawn camera has no roll (its up is always +Y), and a camera looking
  straight up or down is nudged a hair so it stays valid.
- `camera:ViewportPointToRay(x, y, depth?)` and `camera:ScreenPointToRay(x, y, depth?)` return a `Ray` whose `Direction` is a
  unit vector and whose `Origin` is the camera position moved `depth` studs along it. `camera:WorldToViewportPoint(point)` and
  `camera:WorldToScreenPoint(point)` return `Vector3(x, y, depth)` and whether the point is in front of the camera and inside
  the viewport; behind the camera `depth` is negative. `Viewport` coordinates start at the top left of the viewport; `Screen`
  coordinates start below the GUI inset (`Game::set_gui_inset`), so `ScreenPointToRay(x, y)` is `ViewportPointToRay(x, y + inset)`.
- `Ray.new(origin, direction)`: `Origin`, `Direction`, `Unit` (a ray with a unit direction), `ray:ClosestPoint(point)` and
  `ray:Distance(point)` (the ray is the segment from `Origin` to `Origin + Direction`), `==`.
- Hosts: `Game::set_viewport_size` (done by `Game::render` with the size it draws) and `Game::set_gui_inset`; the units are
  whatever the host uses for both.

## Datatypes
- `Vector3.new(x, y, z)`: `X Y Z Magnitude Unit`, `+ - *` (vector * number, number * vector, vector * vector), also `/ number`,
  unary `-`, `==`.
  Components retain the engine's f64 precision in userdata.
- `Vector2.new(x, y)`: `X Y Magnitude Unit`, `+ -`, `*` and `/` (by a number or a `Vector2`), unary `-`, `==`. It is not a
  property type; input actions, `RemoteEvent` and scripts use it. `Vector2.new()` is `(0, 0)`.
- `CFrame`: a position and a rotation (Roblox conventions: the rotation's columns are the right, up and back axes;
  angles in radians). Code: `metallic_core::CFrame` and `crates/metallic_script/src/api/cframe.rs`.
  - Constructors: `CFrame.new()`, `new(pos)`, `new(pos, lookAt)`, `new(x, y, z)`, `new(x, y, z, qx, qy, qz, qw)`,
    `new(x, y, z, R00, R01, R02, R10, R11, R12, R20, R21, R22)`, `CFrame.identity`, `lookAt(at, target, up?)`,
    `Angles(rx, ry, rz)` = `fromEulerAnglesXYZ` (Rx * Ry * Rz: Z is applied first), `fromEulerAnglesYXZ` =
    `fromOrientation` (Ry * Rx * Rz), `fromEulerAngles(rx, ry, rz, order?)` with `Enum.RotationOrder` (default XYZ),
    `fromAxisAngle(axis, angle)`, `fromMatrix(pos, vX, vY, vZ?)`. The 12-number `new` re-orthonormalises its matrix
    the way a property stores it (an orthonormal matrix is kept as given, a scaled or skewed one keeps its look
    vector); a finite matrix that cannot be fixed (zero or parallel columns) gets the identity rotation. `fromMatrix`
    keeps its axes as given.
  - Fields: `Position`, `X Y Z`, `Rotation`, `LookVector`, `RightVector`, `UpVector`, `XVector`, `YVector`, `ZVector`.
  - Methods: `Inverse`, `Lerp(goal, alpha)` (position linear, rotation along the shortest arc), `ToWorldSpace`,
    `ToObjectSpace`, `PointToWorldSpace`, `PointToObjectSpace`, `VectorToWorldSpace`, `VectorToObjectSpace` (each takes
    several values and returns as many), `GetComponents` (also `components`), `ToEulerAnglesXYZ`, `ToEulerAnglesYXZ`,
    `ToOrientation`, `ToEulerAngles(order?)`, `ToAxisAngle`, `Orthonormalize`, `FuzzyEq(other, epsilon?)`.
  - Operators: `CFrame * CFrame`, `CFrame * Vector3` (a point), `CFrame + Vector3`, `CFrame - Vector3`, `==`;
    `tostring` gives the 12 components. Fields cannot be assigned.
- `BasePart.CFrame` reads `Position` and `Orientation` as one frame and writes both. `Orientation` keeps degrees
  applied Y, then X, then Z, so `part.CFrame:ToOrientation()` is `Orientation` in radians. A non-finite CFrame is
  refused and changes nothing. `BasePart.CFrame` itself is not stored; `PivotOffset` and `WorldPivot` are stored
  CFrame properties (scene files keep them). Remote payloads do not carry CFrames yet.
- `Color3.fromRGB(r, g, b)`: `R G B` read as 0..1.
- `UDim2.new(xs, xo, ys, yo)`: `X` and `Y` read as `{Scale, Offset}`.

```lua
part.Color = Color3.fromRGB(255, 80, 0)
part.Size = Vector3.new(4, 1, 2) * 2
```

## Pivots, size and scale
Roblox's `PVInstance` API on `BasePart` and `Model` (`IsA("PVInstance")` is true for both; it is an abstract class above them).
Code: `metallic_core::transform`, shared with Studio's building tools. Spec: `docs/design/building-tools.md` sections 4.3,
4.4 and 16.

- `GetPivot()`: for a part `CFrame * PivotOffset`. For a Model, its `PrimaryPart`'s pivot when that is a BasePart inside
  the model; otherwise `WorldPivot`.
- `PivotTo(cframe)`: moves the part, or every BasePart descendant of the model (through Folders and nested Models),
  so the pivot lands on `cframe`. The WorldPivots of nested Models move with them. On a Model without a PrimaryPart,
  `WorldPivot` becomes `cframe`.
- `BasePart.PivotOffset` (CFrame, default identity): the pivot relative to the part.
- `Model.WorldPivot` (CFrame): until it is first set (directly or by `PivotTo`) it reads as the centre of the model's
  bounding box with world axes, and follows the parts; after that it stays where it was put. It is ignored while a
  valid `PrimaryPart` is set. `Model.PrimaryPart` (BasePart or nil) is ignored when it is not a BasePart inside the model.
- `BasePart:Resize(face, delta)`: `face` is `Enum.NormalId` (`Right Top Back Left Bottom Front`, values 0 to 5; the number
  or the name also work). The face moves `delta` studs along its normal and the opposite face stays; sizes stay within
  0.001 to 2048 studs.
  A Ball grows on all axes, a Cylinder's Y and Z together. Returns false and changes nothing when the part has
  `CanCollide` and the grown part would newly overlap (by more than 0.0001 studs) another `CanCollide` part in
  Workspace; true otherwise.
- `Model:ScaleTo(scale)` / `GetScale()`: `scale` (positive, finite) is absolute, relative to the model's size at scale 1,
  as in Roblox: `ScaleTo(2)` then `ScaleTo(0.5)` gives half the original size, and `ScaleTo(1)` (or
  `ScaleTo(GetScale() * 0.5)` after `ScaleTo(2)`) restores it. Part positions and sizes scale about the pivot, which stays
  put; orientations stay; `PivotOffset` positions and nested WorldPivots scale too; nested Models' `GetScale` multiplies.
- **Limits.** A part's `Size` is 0.001 to 2048 studs per axis. Assigning `Size` clamps each axis, as Roblox does, and
  so does `Resize`. `ScaleTo` and `PivotTo` compute and check every new value first. If a part would leave the size
  limits, or a position would overflow, they error and change nothing (`GetScale` included).
- **CFrame properties.** `PivotOffset`, `WorldPivot`, `BasePart.CFrame` and `PivotTo`'s target are made orthonormal on
  write (every CFrame property is, in `World::set`). A scaled or skewed matrix keeps its right axis and corrects up
  and right (`CFrame:Orthonormalize`), so `PivotTo(cf)` then `GetPivot()` returns `cf:Orthonormalize()`. A degenerate
  rotation (for example all zeros from `fromMatrix`) errors.
- `Model:GetBoundingBox()` returns `(CFrame, Vector3)`: the box around every BasePart descendant, oriented like the
  PrimaryPart (or the pivot when there is none). An empty model returns its pivot and size 0.
  `Model:GetExtentsSize()` returns that size.

```lua
local door = workspace.Door
door.PrimaryPart = door.Hinge
door:PivotTo(door:GetPivot() * CFrame.Angles(0, math.rad(90), 0))   -- swing about the hinge
local cf, size = door:GetBoundingBox()
```

## Signals
`signal:Connect(fn)` returns a connection with `:Disconnect()` and `.Connected`. Handlers run in their own thread (they may
`task.wait`). A `Connect` made inside a handler does not run in the same dispatch; `Disconnect` inside a handler is safe.
Signals are declared in the class files and inherited, so a signal of `BasePart` is on every part.

- `signal:Once(fn)` is a `Connect` that disconnects itself just before its first call (`.Connected` is already false
  inside `fn`). It returns a normal `RBXScriptConnection`; `:Disconnect()` before the signal fires means `fn` never runs.
- `signal:Wait()` yields the calling thread until the signal next fires and returns that fire's arguments (the same
  values a `Connect` handler gets). Every thread waiting on a signal resumes on the same fire, after the handlers, each
  under the normal 0.1 s resume budget. A `Wait()` started during a fire (for example inside a handler of the same
  signal) resumes on the next fire. A wait ends without resuming when its script stops or is destroyed, or when the
  signal's instance is destroyed. Outside a yieldable thread (inside a metamethod, for example) it fails with the same
  error as `task.wait`. Both work on every signal.

| Signal | Arguments | Fired when |
|---|---|---|
| `Instance.Changed` | `propertyName` | a property of the instance really changed (see "Property changes" below) |
| `Instance:GetPropertyChangedSignal(name)` | none | that property really changed |
| `BasePart.Touched` | `hit` | the runtime calls `ScriptHost::touched(part, hit)`; when a character touches the part, `hit` is the character part touching it: a body part (`Head`, `UpperTorso`, `LeftHand`, ...) whose touch volume follows the animated avatar, or the `HumanoidRootPart` for the movement capsule (either way `hit.Parent` is the character Model). Each character part fires its own `Touched`. Two characters' parts touch each other too: each fires `Touched` with the other's part, once per contact |
| `BasePart.TouchEnded` | `hit` | the runtime calls `ScriptHost::touch_ended(part, hit)` when a character part stops touching the part (it separated, the character died, or the character was destroyed or replaced; two characters' touching parts end the same way), and when two ordinary parts that reported `Touched` stop touching (they separated, one was destroyed, or one changed `Size`, `Shape` or `CanCollide`), on both parts that still exist, once. If the other part was destroyed by then, `hit` is the destroyed instance. Pairs with `Touched` |
| `Humanoid.Died` | none | the runtime calls `ScriptHost::humanoid_died` |
| `Player.CharacterAdded` | `model` | the runtime calls `ScriptHost::character_added` (also sets `Player.Character`) |
| `Players.PlayerAdded` | `player` | queued registration on the next step; Combined also registers Player1; pending Added is flushed before active removal |
| `Players.PlayerRemoving` | `player` | `remove_player`, before caller-owned destruction; no synthetic removal on stop |
| `RunService.Heartbeat` | `dt` | every `step` |

```lua
workspace.Lava.Touched:Connect(function(hit)
    local hum = hit.Parent:FindFirstChild("Humanoid")
    if hum then hum:TakeDamage(25) end
end)
local player = game:GetService("Players").PlayerAdded:Wait()
local dt = game:GetService("RunService").Heartbeat:Wait()   -- wait one frame
workspace.Door.Touched:Once(function(hit) print("first touch:", hit.Name) end)
```

### Property changes
`inst:GetPropertyChangedSignal(name)` and `inst.Changed` are ordinary `RBXScriptSignal`s, for every property of every class.
Code: `crates/metallic_script/src/api/property_signals.rs`; tests: `crates/metallic_script/tests/property_changed.rs`.

- They fire for a real change only: a write of the value the property already has fires nothing; `A -> B -> A` is two
  changes. Writes from scripts and from the engine (physics, the character controller, Studio) count alike.
- Delivery is on the next `step`, after inputs and GUI events and before `Heartbeat`, oldest change first; for one change
  `GetPropertyChangedSignal(name)` handlers run before `Changed` handlers. A write made inside a handler fires on the
  following `step`, so handlers that keep changing what they watch cannot recurse. Handlers share the step budget like
  every other handler (deferred when it runs out).
- `Changed` passes the property name as a string. Engine bookkeeping properties (`Model.WorldPivotSet`,
  `Model.ScaleFactor`) never fire it, and asking for them by name errors like an unknown name:
  `Nope is not a valid property name.` Names are case-sensitive, like Roblox.
- Covered: every registry property of the class. A `BasePart.CFrame` write fires `Position` and `Orientation`. Not yet (#437):
  `Parent` and `CFrame` are not class properties until #464, so `GetPropertyChangedSignal("Parent")` and `("CFrame")` give the
  unknown-name error, and `Changed` does not fire for `Parent`.
- Changes made before a script connects or waits are not delivered to it. Destroying the instance drops its connections
  (`Connected` becomes false); a new instance never fires old ones.
- Each VM gets the changes of instances its own scripts listen to; both the server and the client VM see the shared world.
  Listening costs nothing until a script connects, and a write costs nothing extra at any time: changes are read from the
  world's change journal once per step, and only while something listens.
- More than 16384 world changes between two steps while something listens drops that step's property events with a warning.

```lua
local hum = workspace.Npc.Humanoid
hum:GetPropertyChangedSignal("Health"):Connect(function()
    workspace.Npc.Bar.Size = Vector3.new(hum.Health / hum.MaxHealth * 4, 0.3, 0.3)
end)
workspace.Door.Changed:Connect(function(name) print("Door." .. name .. " changed") end)
```

## Services
`game:GetService(name)` (or `game.Name`) for Workspace, Players, ReplicatedStorage, ServerScriptService, StarterPlayer,
StarterGui, Lighting, RunService, UserInputService, ContextActionService. Others error: `'Foo' is not a valid Service name`.

- `Players.LocalPlayer` (client VM; nil on the server), `Players:GetPlayers()` (players whose `PlayerAdded` was delivered),
  `Players:GetPlayerFromCharacter(model)`.
- Player: `UserId`, `Character`, `CharacterAdded`. Combined startup creates a `PlayerGui` for its local player;
  server-only startup adds no player or GUI.
- `Humanoid:TakeDamage(n)` only lowers `Health` (not below 0). Death is detected by the character controller.
- Humanoid tuning is clamped on write: `Health` and `MaxHealth` to 0..1e9, `WalkSpeed` and `JumpPower` to 0..1000,
  `MaxSlopeAngle` to 0..89. Setting a NaN or infinite number raises an error and keeps the old value. A character
  dies when `Health` is 0, when it falls below `Workspace.FallenPartsDestroyHeight`, or when its root goes more than
  1,000,000 studs from the origin on any axis.
- `RunService:IsServer()`, `RunService:IsClient()`, `RunService.Heartbeat`.

```lua
game:GetService("Players").PlayerAdded:Connect(function(player)
    print(player.Name, game:GetService("RunService"):IsServer())
end)
```

## Tasks and coroutines

- `task.spawn(functionOrThread, ...)` resumes immediately; `task.delay(seconds, functionOrThread, ...)` queues
  the original function/thread and complete argument tuple. A thread already queued by the scheduler cannot
  be scheduled again before its current admission completes.
- `task.wait(seconds?)` and `task.delay(nil, callback, ...)` default nil duration to zero. Numeric strings are
  coerced to numbers; negative durations use zero. Invalid duration values raise positioned argument errors.
- While a task thread is queued, `coroutine.resume(thread, ...)` returns false and an ownership error, and a
  wrapped coroutine raises that error. Foreign arguments do not replace the original callback or wake values.
  Cancelled script tasks remain blocked while a user retains their thread handles.
- A user coroutine that calls `task.wait` is adopted by the scheduler and resumes at its due time with the
  elapsed duration. Its initial resume/wrap call exposes no internal scheduler values. Ordinary later yields
  release queued ownership so manual resumes work again, preserving nil/multiple values and native errors.
- Native `pcall` and `xpcall` are unchanged. Ordinary coroutine creation, yielding, status and close behavior
  remain available; internally tagged results are consumed by the scheduler/admission helpers.

## GUI and buttons
The local player's `PlayerGui` (copied from `StarterGui` at start) is drawn over the 3D view: `ScreenGui` (`Enabled`,
`DisplayOrder`), `Frame` (`Position`, `Size`, `BackgroundColor3`, `BackgroundTransparency`, `Visible`, `ZIndex`, `Active`,
`ClipsDescendants`), `TextLabel` (`Text`, `TextColor3`, `TextSize`) and the buttons `TextButton` (`Text`, `TextColor3`,
`TextSize`) and `ImageButton` (`Image`, `ImageColor3`, `ImageTransparency`; the image is not drawn yet). Both buttons
are `GuiButton`s (`AutoButtonColor`), which are `Frame`s here (Roblox's GuiObject).

- Order: ScreenGuis by `DisplayOrder`, then each element above its parent and siblings by `ZIndex` (Roblox's default
  `ZIndexBehavior.Sibling`); ties keep tree order. `ClipsDescendants` cuts its descendants to its rectangle, for drawing
  and clicks.
- A press goes to the front-most element under the pointer that takes clicks: a button, or an element with
  `Active = true`. Other elements let it through. A press taken by the GUI never reaches the game (camera, jump,
  `ContextActionService`).
- Button events: `MouseButton1Down(x, y)`, then on release over the same button `MouseButton1Up(x, y)`,
  `MouseButton1Click()` and `Activated(inputObject, clickCount)`. A finger tap fires the same events. `AutoButtonColor`
  darkens the background while hovered or pressed.
- Every Frame: `MouseEnter(x, y)` and `MouseLeave(x, y)` (mouse only), `InputBegan(input)`, `InputChanged(input)` (mouse
  movement, at most once per frame) and `InputEnded(input)` for the elements under the pointer down to the first that
  takes clicks. `input` has `UserInputType` (`MouseButton1`, `MouseMovement` or `Touch`), `UserInputState`, `Position`
  (Vector3, GUI points), `Delta` and `KeyCode`.
- Events reach LocalScripts at the next step; at most 256 wait at once. Not yet: gamepad selection, `Modal`,
  `MouseButton2` events, double-click counts, `ZIndexBehavior.Global`.

```lua
local button = script.Parent.Play
button.Activated:Connect(function(input)
    print("play pressed with", input.UserInputType.Name)
end)
```

## RemoteEvent
`FireServer(...)` (client only), `FireClient(player, ...)` and `FireAllClients(...)` (server only), `OnServerEvent(player, ...)`
(server only), `OnClientEvent(...)` (client only). Wrong side: `FireClient can only be called from the server` and so on.

- Allowed values: nil, boolean, number, string, Vector3, Vector2, Color3, UDim2, Instance, and tables of these up to 16 levels deep.
  They are deep-copied (each handler gets its own copy); Instances pass by reference (a destroyed one arrives as nil).
- Delivery is on the next `step`, never in the same call.
- Refused: `cannot send function` (also thread/other userdata), `cannot send cyclic table`, tables deeper than 16 levels.

```lua
-- server
remote.OnServerEvent:Connect(function(player, amount) print(player.Name, amount) end)
-- client
remote:FireServer(5)
```

## Limits
- Pattern matching: the 10 million charged-step complexity allowance applies separately to each starting-position
  attempt. Deadline polling continues across attempts and substitutions; the script's 0.1 s execution budget still
  applies to the complete call. A larger linear operation can therefore time out even when it passes the complexity
  check. The separate 200-level matcher depth, 32-capture and 64 MB gsub-result limits are unchanged.
- Remote: at most 32 arguments, 10,000 values (tables and leaves, shared tables counted per reference) and 1 MB of string bytes per
  call (`remote payload too large`); at most 1,000 messages and 16 MB queued per VM per step (`remote queue full`).
- Signals: at most 10,000 connections per signal (`too many connections`). All handlers of a step share a 0.25 s budget per VM;
  the rest run on the next step, in order. A script whose handlers time out 3 times in a row is disconnected (error printed once).
- A script that is destroyed, disabled (`Enabled = false`) or moved out of a place where scripts run stops: its connections and
  threads are removed. Ownership is tracked by the script's instance id, so clones with the same full name stop independently.
- Userdata metatables are protected (`getmetatable` returns false, `setmetatable` errors).
- Errors raised by API calls and operators are plain strings with `<script>:<line>:` so `pcall` shows them cleanly.

## Milestone 1 limits
In combined mode the client VM writes to the same World as the server VM (single process). Nothing stops a LocalScript from changing server-owned
state; M2 replication will separate them. Accepted for M1.

## Input (client only)
Spec: `docs/design/input-actions.md` section 5. Everything here needs the client VM (a `LocalScript`).

### Input enums
See [Enums](#enums). `Enum.KeyCode` (`Unknown`, letters, digits, `F1` to `F12`, `Space`, arrows, `MouseButton1`, `ButtonA`, `Thumbstick1`, ...: the
controls of the input runtime), `Enum.UserInputType` (`MouseButton1` to `3`, `MouseWheel`, `MouseMovement`, `Touch`, `Keyboard`,
`Focus`, `Gamepad1` to `4`, `TextInput`, `None`), `Enum.UserInputState` (`Begin`, `Change`, `End`, `Cancel`, `None`),
`Enum.InputActionType`, `Enum.InputTrigger`, `Enum.TouchSlot` (Metallic), `Enum.ContextActionResult` (`Sink`, `Pass`),
`Enum.PreferredInput` (`KeyboardAndMouse`, `Gamepad`, `Touch`), `Enum.RotationOrder` (`XYZ`, `XZY`, `YZX`, `YXZ`, `ZXY`, `ZYX`; used by `CFrame`), `Enum.NormalId` (used by `Resize`), `Enum.ContextActionPriority` (`Low` 1000, `Medium` 2000,
`High` 3000). Raw `UserInputService` events (#80) will reuse these names.

### InputAction
`GetState()` (boolean, number, `Vector2` or `Vector3` by the action's `Type`; zero or false when the action or its context is
disabled), `GetHoldProgress()` (0 to 1), and the signals `Pressed()`, `Released()`, `Triggered(value)`, `StateChanged(value)`.
The value is the action's value as the runtime reported it. On the server any of these errors with
`InputAction can only be used from a LocalScript`; other members still work.

### ContextActionService
- `BindAction(name, handler, createTouchButton, ...inputs)` and `BindActionAtPriority(name, handler, createTouchButton,
  priority, ...inputs)`. Inputs are `Enum.KeyCode` or `Enum.UserInputType` values (`Keyboard` binds every key, `Gamepad1` to
  `4` bind the gamepad buttons, `MouseMovement` binds `MouseDelta`). The call creates a hidden `InputAction` (one binding per
  key) in a hidden `InputContext` named `ContextActionService@<priority>` under the local player; binding a name again
  replaces it only once the new one is complete (a failed call changes nothing). A script may have at most 256 bound
  actions; past that `BindAction` errors with `too many bound actions`. Default priority is 2000.
- `handler(actionName, state, inputObject)` runs with `Begin`, `End`, `Change` (axis actions) or `Cancel` (the action was
  unbound or its context disabled while pressed). `inputObject` has `KeyCode` (`Unknown`), `UserInputType`, `UserInputState`,
  `Position`, `Delta`. Return `Enum.ContextActionResult.Pass` to let lower consumers see the event; anything else sinks.
- `UnbindAction(name)`, `SetTitle(name, text)`, `SetImage(name, image)`, `SetPosition(name, udim2)` (set `TouchLabel`,
  `TouchImage`, and `TouchLayout = Custom` with `TouchPosition`). `GetButton(name)` returns the touch overlay's button of an
  action bound with `createTouchButton`, or nil when there is none or the overlay did not place it. It is a read-only
  `TouchButton` object (the overlay is drawn by the Player, not by GUI instances): `Name`, `ClassName`, `Text`, `Image`,
  `Visible`, `AbsolutePosition` and `AbsoluteSize` (`Vector2`, dp from the top-left of the screen), `Position` and `Size`
  (`UDim2` offsets in dp).

### UserInputService (last input only)
`GetLastInputType()`, `LastInputTypeChanged(type)`, `PreferredInput`, `TouchEnabled`, `KeyboardEnabled`, `GamepadEnabled`.
Raw events stay with #80.

### Host side
`ScriptHost::input_action_event`, `set_action_state`, `set_input_devices`, `set_preferred_input`, `set_last_input_type` and
`take_bound_results` connect the input runtime to the scripts.

### Limits
Handlers share the 0.25 s step budget and run under the 0.1 s resume budget like all handlers. At most 16 action events are
dispatched per `step`, oldest event time first; the rest wait for the next steps (the queue holds 1024, the oldest are
dropped beyond that).

```lua
local dash = game:GetService("ReplicatedStorage").Input.Gameplay.Dash
dash.Triggered:Connect(function() request:FireServer() end)   -- send the intent, the server checks the cooldown
```
