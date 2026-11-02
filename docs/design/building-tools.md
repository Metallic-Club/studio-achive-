# Building tools: Studio design spec

Date: 2026-10-30. Status: **design** (docs only; nothing here is implemented). Issue #200.

Research behind every choice: `docs/research/2026-10-30-building-tools.md` ("research" below; section numbers like
R3.2 point into it). This spec builds on:

- `docs/research/2026-10-30-studio-ui-design.md` ("UI doc"): layout, viewport header, gizmo drawing, Ctrl K, Shift A,
  accessibility rules, Metallic keymap.
- `docs/design/look-presets.md` ("Look spec"): Looks, material classes, frame order, the GPU material table plan.
- `docs/design/visual-language/` (merged from PR #199): tokens and components. The mockups use `tokens.css` values.
- Task 10 (#10) in `docs/superpowers/plans/2026-10-30-engine-m1.md`: `EditorState`, `Command`, `History`.

Mockups: `docs/design/building-tools/` (section 18).

Labels: **Assistant's call** marks a decision the owner can overturn (all listed again in section 19). Numbers marked
"start value" are starting points for the first implementation to tune.

## 0. Decisions in short

1. **One drag, one undo step, everywhere.** Gizmo drags, colour-wheel drags, slider drags, brush strokes, array and
   mirror commits each become one `Command` on release. Live preview during the gesture never touches `History`.
2. **The object itself is the main handle.** Dragging a part moves it along the surface under the cursor (Roblox's
   dragger habit) with the grid applied in the surface plane. The gizmo is for axis-locked work.
3. **Type while you drag.** During any gizmo drag, typing a number sets the exact amount ("5 Enter" moves 5 studs on
   the dragged axis). Blender and SketchUp do this (R4.9, R4.10); Roblox offers an editable distance box only
   after the move ends (R2.1), and Metallic keeps that too.
4. **A contextual toolbar follows the selection** with the eight things people do to a selected part: tool, snap,
   colour, material, style, align, mirror, array. Every button has a single-key shortcut shown in its tooltip.
5. **Per-part style is a property, `LookOverride`, on `BasePart` and `Model`.** It changes only what can differ per
   draw (shading model, vertex snap, affine UVs, nearest sampling, blocky detail, hull outline) plus three masked
   screen effects (pixelate, palette quantise, edge lines). Lighting, tonemapper, exposure, bloom and the place's
   render scale stay place-wide.
6. **The colour popover is an HSV ring with a square**, RGB/HSV/Hex fields, an eyedropper that reads the part's
   `Color` (not the lit pixel), recent and saved swatches, OKLCh-based palette generation, and full keyboard control.
7. **The runtime build kit is a Luau package** (`BuildKit`) plus three engine classes Roblox games already know:
   `Handles`, `ArcHandles` and `DragDetector`. Game creators insert it; the Build Sandbox template (#89) uses it.

## 1. Scope and non-goals

In scope: placing and arranging Parts and Models in Studio and at run time; colour, material and per-part style.

Not here:
- Mesh editing (faces, edges, extrude, booleans): the modeller, #136 and epic #19. Section 5.4 says where its snapping
  hooks in.
- Shapes: #35 (`metallic_meshgen`).
- Terrain sculpt and paint: #175. The brush in section 9 shares its radius and falloff widget.
- Keymap storage and preset switching: #129. Section 15 lists the building rows it must hold.
- Command palette and Insert palette: #128. Every command named here registers with it.
- CSG (Union, Negate, Intersect, Separate): the research (R2.5) shows it is heavily used on Roblox, but it needs the
  modeller's mesh booleans (#136, #147). It gets a follow-up once #136 lands; the keys are reserved in section 15.

## 2. Shared rules

### 2.1 Commands and undo

- Every tool produces one `Command` per committed gesture through `History::apply`. The plan's `SetProperty` grows a
  batch form, `SetProperties(Vec<(InstanceId, PropName, Value, Value)>)` (old and new value), so moving 500 parts is
  one entry, not 500.
- Undo names are "Verb object (count)": "Move 12 parts", "Colour: #5280CC (3 parts)", "Style: Retro (1 model)".
  Edit > Undo shows the name; the status bar flashes it for 2 s after Ctrl+Z.
- During a gesture the tool writes to a **preview overlay**: a map `InstanceId -> {prop: value}` that the renderer
  and Properties read on top of the world. Release turns the overlay into the command; Esc or right-click during the
  drag clears it. Scripts and physics never see the overlay (Studio edit mode has no running scripts anyway; Build in
  Play, section 13, writes to the play copy directly).

### 2.2 Units and increments

| Setting | Default | Steps offered | Keys (Metallic preset) |
|---|---|---|---|
| Move increment | 1 stud | 0.05, 0.1, 0.2, 0.25, 0.5, 1, 2, 4, 8, 16 | `[` halve, `]` double (Hammer and TrenchBroom habit, R4.6, R4.7) |
| Rotate increment | 15 degrees | 1, 5, 11.25, 15, 22.5, 30, 45, 90 | Shift+`[`, Shift+`]` |
| Scale increment | follows move | same as move | |
| Snapping on | yes | | header toggle; Ctrl inverts while dragging (UI doc 5.5) |
| Precision | | | Shift while dragging: one tenth speed (UI doc 5.5) |

- Increments are per place, saved in the place's Studio settings (not in the scene file), so a 0.2-stud detail
  project keeps its grid when you switch to a 1-stud project. Assistant's call.
- Roblox's increments sit in the toolbar with Shift+2 and Alt+R to focus them (R2.2); the Roblox Studio keymap
  preset keeps those keys (section 15).
- The grid's origin is world zero. On each axis, a part (or selection) snaps its **bounding-box min corner** to the
  grid, so its faces sit on grid lines whenever its size is a multiple of the increment. This applies to surface
  placement, Snap to grid and the "Off grid" indicator. Arrow-key and handle drags snap the amount moved, not the
  position.
- Why not the centre: an earlier rule snapped the centre when the size was a multiple of the increment. For an odd
  multiple (a 1-stud cube on a 1-stud grid) that put the faces half a step off the grid lines, contradicting the
  "flush" goal, and flagged the templates' flush parts as off grid. The min-corner rule gives the same result for even
  multiples and is right for odd ones and for sizes that are not multiples.

### 2.3 What counts as the selection

- The selection is a list of `InstanceId`; the **active** item is the last one clicked (drawn with a 2 px accent
  silhouette; others 2 px `accent` at 70% alpha). Align and Resize-to-fill use the active item as the reference.
- A Model in the selection counts as one object for transforms: it moves by its pivot (section 4.4).
- Locked parts (`Locked` property, Roblox name) cannot be clicked in the viewport but can be selected in Explorer.

## 3. Viewport surfaces

Layout (mockup `viewport-toolbar.html`):

```
+--------------------------------------------------------------------------------------------+
| viewport header 36 px: [Q W E R T] Move · 1 stud · 15° · World · Pivot · Snap: Grid Surface |
+--------------------------------------------------------------------------------------------+
|                                                                                            |
|                       [ contextual toolbar, 36 px, 12 px above selection ]                 |
|                               +----------------+                                           |
|                               |   selection    |  <- gizmo at pivot                        |
|                               +----------------+                                           |
|  hint card (bottom left, max 1)                                     status: 12 parts · 4x1x2|
+--------------------------------------------------------------------------------------------+
```

### 3.1 Viewport header (tool settings)

From the UI doc 5.5, extended:

- Tool segment: Select (Q), Move (W), Rotate (E), Scale (R), Transform (T: move arrows, rotate rings and scale cubes
  together, Roblox's Transform tool and Unity's T tool, R2.1).
- Increment chips: "1 stud", "15°". Click opens a list of the steps in 2.2; scroll on the chip steps through them.
- Coordinate space: World / Local (X toggles).
- Pivot: Pivot / Centre (Z toggles; Unity's key, R4.1). Pivot uses the object's pivot (4.4); Centre uses the
  selection's bounding-box centre.
- Snap targets: four toggle chips, Grid, Surface, Vertex, Edge. Holding V forces vertex and edge snapping for one
  drag whatever the chips say (Unity and Unreal habit, R4.1, R4.4).
- Collisions: off by default. On, a drag stops where the moved part would overlap another (Roblox's Collisions
  toggle, R2.8). Shown as a chip so people see why a part refuses to move.

### 3.2 Contextual toolbar

A floating bar that follows the selection. It answers the most common complaint in the research: Roblox spreads
colour, material, align and pivot across ribbon tabs far from the part (R2.9), so people install F3X to get one panel
(R1.1).

- **Contents, left to right**: tool icon (current of Move, Rotate, Scale); increment chip; colour swatch (C);
  material thumbnail (M); style chip (L); Align (A); Mirror (Ctrl+M); Array (Ctrl+Shift+D); "More" (lock, anchor,
  collide, group, tag). 36 px high, `bg.raised`, radius `r-md` 6 px, `shadow-pop`, 1 px top highlight. Icons 16 px in
  `icon`; the swatch shows the actual colour in a 16 px square with a 1 px `line-strong` ring.
- **Popovers** from the toolbar open under their button, or above it when the toolbar sits low, and slide sideways
  (up to 12 px from the viewport edge) to cover as little of the selection as possible, so the live preview stays
  visible. They never cover the toolbar itself.
- **Placement**: centred horizontally on the selection's screen-space bounding box, 12 px above it. If that is off
  screen or overlaps the gizmo, it goes 12 px below; if both fail, it docks to the viewport's top edge under the
  header. It never covers the selection.
- **When it shows**: after a selection change, once the pointer is still for 200 ms. It hides during gizmo drags,
  camera flight, box selection and Play. It does not animate in position; it appears with a 90 ms fade (reduced
  motion: instant).
- **Mixed selection**: the swatch shows a split square and "Mixed" in its tooltip; a click still opens the popover.
- **Keyboard**: Ctrl+. focuses the toolbar (VS Code's quick-actions key); Left and Right move between buttons; Enter
  opens; Esc returns focus to the viewport. F6 also cycles to it as a panel. Every button has an AccessKit role
  (button or toggle), a label ("Colour, currently #5280CC") and its shortcut.
- **Turn off**: View > Contextual toolbar (default on). The header and Properties still reach every action.

### 3.3 Status line

The status bar's right side shows, for the selection: count, size of the bounding box in studs ("4 x 1 x 2"), and
during a drag the delta ("+5.0 Y", "35°", "x1.5"). Values are JetBrains Mono (visual language 3.5).

## 4. Transform gizmos

The UI doc 5.5 defines the look (constant 90 px axis length, letters at tips, hover 2.5 to 3.5 px, angle labels).
This section defines behaviour.

### 4.1 Move

- **Axis arrows**: drag moves along the axis in the current space. Snapped to the move increment, measured from the
  start position, so a part at 0.3 moves to 1.3, 2.3 (4.6 puts it back on the grid).
- **Plane squares** (12 x 12 px, 24 px from the origin along both axes): move in that plane.
- **Centre disc** (8 px): free move in the screen plane.
- **Dragging the part body** (any tool, including Select): the part follows the surface under the cursor
  (section 5.2).
- **Axis lock during any drag**: press X, Y or Z to lock movement to that world axis, Shift+X/Y/Z to lock to the plane
  normal to it, the same key again to unlock (Blender and Axiom habit, R4.9, R3.2). While a drag is held these keys
  have only this meaning.
- **Alt+drag on an arrow or the body duplicates first** and moves the copy (Unreal and TrenchBroom habit, R4.4,
  R4.7). The duplicate plus move is one undo step "Duplicate and move".
- **Ctrl+D during a body drag stamps**: it leaves a copy where the selection is and keeps dragging (Roblox's stamping,
  R2.1). Each stamp is one undo step.
- **Rotate while dragging the body**: R turns 90 degrees about the hovered surface's normal, T tilts 90 degrees
  toward the camera. Same keys as Roblox Studio's cursor drag (R2.1), Bloxburg and Build a Boat (R3.8, R3.9).
- **End drops the selection** straight down onto the first surface below it (Unreal habit, R4.4). One undo step.
- **Space, held, brings the gizmo to the cursor** so a handle is under the mouse without moving the camera (Roblox's
  handle summoning on Tab, R2.1; section 15 explains the key). Release puts it back at the pivot.

### 4.2 Rotate

- Three axis rings plus an outer screen-space ring (UI doc). Drag snaps to the rotate increment, relative to the start
  angle. The swept arc fills at 22% alpha; the label shows the signed angle.
- Rotation is about the pivot (4.4) or the selection centre (header Pivot/Centre).
- Ctrl+R and Ctrl+T on a selection (no drag): rotate 90 degrees about up, tilt 90 degrees about the camera's right
  axis. One undo step each. Roblox binds the same keys (R2.1).

### 4.3 Scale (Resize)

- **Face handles** (cubes on each axis, both directions): drag resizes that face only; the opposite face stays put.
  This is Roblox's Scale tool behaviour (R2.1) and what `BasePart:Resize` does. Snapped to the scale increment on the
  part's size, not its position.
- **Alt while dragging a face**: both opposite faces move (size changes twice as fast, centre fixed). The Roblox
  Studio preset maps Alt to uniform scale and Ctrl to scale about the pivot instead, as Roblox does since 2024 (R2.1);
  Roblox users still press Shift for uniform scale out of old habit (R2.12), so the preset follows current Roblox.
- **Corner handle** (one, on the +X+Y+Z corner in local space, a 9 px square): uniform scale of the whole selection
  about the pivot. Models scale with `Model:ScaleTo` semantics (positions and sizes of children together).
- Sizes stay inside core's part limits, 0.001 to 2048 studs on each axis (`metallic_core::transform`), so grabbing a
  handle never changes a size by itself. A part never flips inside out; dragging past zero clamps.
- Ball parts keep equal axes; any face handle scales all three (Roblox behaviour).

### 4.4 Pivot

- Every BasePart has `PivotOffset` (CFrame relative to the part); every Model has `WorldPivot`. Defaults: part centre;
  for a Model, the centre of its bounding box at the time it was grouped. Roblox names and behaviour (R2.3).
- **Edit pivot** (P toggles; also a header button): the gizmo moves and rotates the pivot only. While editing, the
  pivot snaps to the selection's bounding-box corners, edge midpoints, face centres and centre (27 candidate points),
  and with V to any vertex. Candidate points show as 6 px silver dots; the hovered one fills with accent.
- "Reset pivot" (palette and pivot right-click menu) restores the default. One undo step.
- Moving a Model with the gizmo uses `PivotTo` semantics: the pivot lands on the snapped position, children follow.

### 4.5 Typed values during a drag

- While a gizmo drag is held, typing digits, `.`, `-`, or an expression (`3*4`, `+2`) opens a small field at the
  cursor showing the typed text and the axis ("Y: 5"). Enter commits exactly that value along the dragged axis (studs,
  degrees, or size delta); Esc cancels the whole drag. Releasing the mouse without Enter ignores the typed text.
- After release, the delta stays on the status line and next to the selection for 5 s as an editable field (Roblox's
  distance box, R2.1): click it or press Enter and type to change the last move, rotate or resize. The change merges
  into the same undo step.
- Models: Blender's typed transform (`G X 5`) and SketchUp's Measurements box (R4.9, R4.10). Assistant's call to
  allow it during the drag as well as after.

### 4.6 Snap to grid absolute

Relative snapping (from the start position) keeps off-grid parts off-grid. When a selected part is off the grid, the
increment chip shows a small "Off grid" label in `warning` text (words, not only colour) and the contextual toolbar
offers "Snap to grid". The command (also Ctrl+Shift+Home in the Metallic preset) moves each selected part's reference
point (2.2) to the nearest grid point and rounds its rotation to the rotate increment. One undo step. Dreams turns
its grid orange for off-grid objects and aligns them with one button (R3.5).

## 5. Snapping

### 5.1 Targets and priority

During a drag the snapper collects candidates in a 12 px screen radius (start value) and picks the first that exists:

| Priority | Target | Glyph (8 px, silver with a 1 px graphite ring) | Source |
|---|---|---|---|
| 1 | Vertex (box corners, mesh vertices) | filled square | V held, or Vertex chip on |
| 2 | Edge midpoint | filled triangle | Edge chip on, or V held |
| 3 | Face centre | ring | Edge chip on, or V held |
| 4 | Point on edge | small diamond | Edge chip on |
| 5 | Surface under cursor | crosshair | Surface chip on (default) |
| 6 | Grid | grid lines brighten in a 4-cell patch | Grid chip on (default) |

Glyph shapes differ so colour is never the only signal. The snapped target's name shows in the status line
("Vertex of Wall_03").

### 5.2 Surface snapping (body drag)

- A ray from the cursor, ignoring the dragged parts, hits a surface. The dragged selection's bounding box is placed so
  its face toward the hit surface touches it. With "Align to surface" (header chip, default **off** for Parts and
  **on** for brush and Insert), the selection also rotates so its local up matches the surface normal.
- On the hit surface, the position snaps to the move increment **in that surface's local grid** (origin at the hit
  part's pivot, axes from its rotation). A part dropped on a 30-degree ramp then snaps along the ramp, not to world
  stud lines. Roblox's 2020 draggers place the bounding box on the hovered surface the same way, walls and ceilings
  included (R2.2); Unreal's surface snap adds the optional rotate-to-normal (R4.4).
- Nothing under the cursor: the selection moves on a virtual plane that extends the last surface it touched (Roblox's
  "virtual surface", R2.2), or on a horizontal plane through its start height if it touched none.
- Soft snapping: with every snap chip off, the drag still pulls to a vertex or edge midpoint within 6 px (half the
  normal radius). Settings > Building > "Soft snap" turns it off; Roblox had to add that switch after complaints
  (R2.12).

### 5.3 Vertex and edge snapping (V)

- Hold V, start a drag on the selection: the nearest vertex **of the selection** to the cursor becomes the grab point
  (a filled square appears there). Moving over another part shows that part's nearest vertex; release puts the grab
  point exactly on it. Unity's and Unreal's V (R4.1, R4.4); Roblox's soft snapping does the same without a key
  (R2.2).
- V+Shift snaps the grab vertex onto a surface instead of a vertex (Unity's V with Shift+Ctrl, R4.1).
- Box parts give 8 corners, 12 edge midpoints, 6 face centres. Meshes give their vertices through the same BVH used
  for picking; a mesh over 20,000 vertices offers only its bounding-box points (performance budget, section 17).

### 5.4 Hooks for the modeller

`metallic_modeling` (#136) consumes the same snapper through a `SnapSource` trait: `candidates(ray, radius_px) ->
Vec<SnapCandidate { point, kind, owner }>`. The modeller adds its own vertices and edges as a source; the glyphs and
priorities stay the same.

## 6. Align, distribute, resize to fill, gap fill

One popover, opened with A or the toolbar's Align button. No mockup yet; it uses the popover frame of 11.1.

### 6.1 Align

- Three rows (X, Y, Z in axis letters and colours), each with Min, Centre, Max buttons. Above them: "Relative to"
  segment: **Active object** (default) / Selection bounds / Grid.
- A click aligns immediately (one undo step) and keeps the popover open. Hovering a button for 150 ms previews the
  result as silhouettes (2D ghost outlines), Roblox's Align Tool and Figma's habits (R2.4).
- In Local space (X toggle) the axes are the active object's.

### 6.2 Distribute

Under Align: "Distribute X / Y / Z" with two modes: **equal centres** and **equal gaps**, plus a "Gap" number field
(empty = keep the current span, a value = set that exact gap and grow or shrink the span). Needs 3 or more objects;
with 2 the buttons are disabled and the tooltip says "Select 3 or more".

### 6.3 Resize to fill (ResizeAlign)

Stravant's ResizeAlign (R1.4) as a two-click mode inside the popover's "Fill" tab:

1. Click a face of part A (the face highlights with a 2 px accent outline and an arrow along its normal). Hovering
   within 25% of a face's edge picks the face behind that edge instead (ResizeAlign's "reach around"), so the back
   face of a wall needs no camera move; the highlight shows which face will be picked before the click.
2. Click a face of part B. A status line under the popover says what to click next ("Click the face to meet").
3. The mode segment decides the result: **Outer touch** (default; both faces extend until they meet at their outermost
   points), **Inner touch** (meet at the innermost points), **Wedge join** (inner touch plus a WedgePart that fills
   the corner, so no Z-fighting), **Extend up to** (only A moves). These are four of ResizeAlign's seven modes; the
   other three (Rounded, Butt, Extend into) wait for user requests.
4. One undo step. Esc at step 2 cancels. Holding Ctrl borrows the normal select drag without leaving the mode (the
   plugin's "Dragger Mode").

The keyboard path: with exactly two parts selected, "Fill: active face to other part" runs it with the faces that
face each other most directly (largest dot product of opposing normals).

### 6.4 Gap fill

GapFill's behaviour (R1.5): pick two edges (V-snap style edge highlight), Studio creates the part or pair of wedges
that closes the gap between them, matching the first part's colour, material and `LookOverride`. Thickness: **Best
guess** (default: the thinner of the two adjacent parts), 1 stud, Thinnest (0.05), or Custom; the same four options
as the plugin. Clicking the same edge twice extrudes that face by the thickness (also from the plugin). Edges must be
within 30 degrees of parallel; otherwise the hint says "Pick two edges that roughly face each other". Assistant's
call: v1 creates Parts and WedgeParts only (no mesh).

## 7. Mirror

- **Ctrl+M** mirrors the selection in place across a plane; **Ctrl+Shift+M** makes a mirrored copy. The plane goes
  through the pivot (or the active object's pivot in Active mode), normal to the axis picked in a three-button popover
  that opens on first use and remembers the last axis (Model Reflect's flow, R1.6).
- Position: reflected. Orientation: a reflection is not a rotation, so Studio reflects the rotation matrix and then
  flips one local axis back to keep it a proper rotation. The axis to flip is the one the shape is symmetric across:
  any axis for Block, Ball and Cylinder; local X for WedgePart (its profile lies in Y-Z); for CornerWedgePart, it
  swaps local X and Z sizes and rotates 90 degrees about Y. MeshParts (later) mirror only if they carry a
  `Mirrorable` attribute; otherwise Studio warns and skips them.
- Models mirror their children and their pivot. Welds and constraints keep their attachments mirrored.
- Live symmetry (edits mirrored as you work, Axiom and Blender style, R3.2) is not in v1; it needs linked copies.

## 8. Duplicate, array, path

### 8.1 Smart duplicate

Ctrl+D duplicates in place and selects the copy (plan's `duplicate_offsets_and_selects_copy`). If the user moves the
copy and presses Ctrl+D again within the same selection, the next copy repeats the same offset and rotation
(Illustrator's "Transform again" and SketchUp's array copy, R4.10). Any other edit or selection change resets it.
The hint system teaches it after the second manual duplicate-and-move (section 14.1).

### 8.2 Array popover (Ctrl+Shift+D)

| Mode | Fields | Default |
|---|---|---|
| Linear | Count, Offset X/Y/Z, "Offset is: Gap / Distance" | Count 5; offset = last smart-duplicate offset, else bounding-box size on the dominant camera axis plus 0 gap |
| Radial | Count, Angle (total), Axis, Centre (Pivot / Active / Picked point), Rotate copies (on) | Count 8, 360°, Y, Pivot |
| Grid | Count X, Y, Z, spacing | 3 x 1 x 3 |
| Along path | Path (pick parts in order, or click points), Spacing or Count, Align to path (on), Up axis | Spacing = selection length + 0 |

- **Drag to add copies**: in Linear mode a handle (a 10 px ring) sits past the last ghost. Dragging it along the axis
  adds or removes copies at the current spacing; the wheel over the handle changes the count by one. Redupe and
  Axiom work this way and users call it more intuitive than dialog arrays (R1.11, R3.2).
- Fields accept SketchUp's and Redupe's short forms: `5x` in Count multiplies by the offset; `/7` in Radial's angle
  makes 7 copies evenly around the full circle (R4.10, R1.11).
- Ghosts preview at 60% opacity silhouettes while fields change. Commit (Enter or "Make 5 copies") is one undo
  step and groups the result under a Model named "Array" when "Group result" is on (default on).
- Along path: the path is a polyline through the picked points, smoothed as a centripetal Catmull-Rom curve; copies
  sit at equal arc length. Assistant's call (centripetal avoids loops at sharp corners).
- v1 is destructive (plain copies). Live, linked arrays (Dreams' Live Clone) are listed under "not adopted now" in
  research R6; they need a linking model first.

## 9. Brush placement (scatter)

B selects the Brush tool. The header shows its settings; the left dock's Assets tab shows the brush set.

- **Brush set**: up to 16 Models or Parts dragged into the brush tray (from Explorer or Assets), each with a weight
  (1 to 10). The brush picks one per placement by weight.
- **Settings**: Radius (Ctrl+wheel or `[`/`]` while B is active; 1 to 256 studs, default 16), Density (instances per
  100 square studs, 0.1 to 50, default 2), Min spacing (default 0.5 x the smallest bounding-box radius in the set),
  Scale range (0.8 to 1.2), Yaw (random 0 to 360), Align to normal (0 to 100%, default 0 for props, 100 for rocks),
  Max slope (0 to 90 degrees, default 35), Surface filter (any / by material / by tag), Parent (a Folder, default
  `Workspace.Brush`).
- **Stroke**: left-drag paints. Points come from Poisson-disk sampling inside the brush disk projected onto surfaces
  (rays down the view direction, then along the hit normal). A stroke adds at most `density x area` per 0.1 s dwell, so
  holding still fills an area to density and stops. Shift-drag erases only instances this brush placed (they carry the
  tag `Brush:<set name>`). One stroke is one undo step.
- **Preview**: the brush disk is a 1.5 px silver circle on the surface with a 1 px inner ring at the falloff edge;
  rejected spots (slope, filter) show nothing (no red marks to read).
- Terrain (#175) reuses this radius and falloff widget and its keys.

Polybrush's prefab scatter and Roblox's Brushtool plugin are the models (R1.7, R4.3).

## 10. Selection and measure

### 10.1 Selection tools

- Q selects the Select tool; pressing Q again cycles its mode: **Box → Lasso → Paint** (Photoshop's Shift+L cycle,
  adapted). The header shows the mode.
- Box: drag on empty space. Parts fully inside are selected; hold Alt (after the drag starts) to include touching
  parts. Shift adds, Ctrl removes (Ctrl-click still toggles one, UI doc).
- Lasso: freehand polygon; a part is in if its screen-space centre is inside (fast and predictable).
- Paint select: a 24 px screen circle selects what it passes over.
- **Select similar** (Shift+G, menu with keys): Same Color (C), Same Material (M), Same Style (L), Same Size (S), Same
  Class (K), Same Name (N), Same Tags (T), Same Parent (P). Applies to the whole place by default, or inside the
  current box when one is drawn. Blender's Select Similar is the model (R4.9).
- **By tag**: the Tags section in Properties (#78's tags) lists the selection's tags; clicking a tag's "Select all"
  selects every Instance with it. Tag Editor's habit, now built into Roblox Studio (R1.9).
- Ctrl+A selects all in Workspace; "Invert selection" (Ctrl K, Edit menu) inverts within Workspace; Esc clears.
  Ctrl+I stays free because the UI doc gives it to Litha.

### 10.2 Measure

- Shift+M toggles Measure. Click point A, click point B (both snap per section 5). A line with end glyphs shows the
  distance in studs with X, Y, Z deltas in axis colours and letters, and the angle to the ground. Measurements stay
  until Esc or the tool changes; up to 8 at once. They are not saved.
- **Distance readouts**: with one or more parts selected, holding Alt and hovering another part shows the gaps
  between their bounding boxes on each axis (Figma's Alt habit, R4.11). Alt+drag still orbits; the readout only shows
  while no button is down.

## 11. Colour

Mockup: `colour-popover.html`.

### 11.1 Popover layout (320 x 476 px)

```
+-------------------------------------------+
| Colour            [eyedropper I]   [x]    |
|  +-----------------------------------+    |
|  |  hue ring 248 px, 18 px thick     |    |
|  |     +---------------------+       |    |
|  |     | SV square 146 px    |       |    |
|  |     +---------------------+       |    |
|  +-----------------------------------+    |
|  [old | new]  Hex [#5280CC]               |
|  R [ 82] G [128] B [204]                  |
|  H [218] S [ 60] V [ 80]                  |
|  Recent  ■ ■ ■ ■ ■ ■ ■ ■ ■ ■ ■ ■          |
|  Palette "Castle" ■ ■ ■ ■ ■ ■ [+]  [v]    |
|  Harmony [Analogous v] ■ ■ ■ ■ ■          |
+-------------------------------------------+
```

- **Ring and square, not triangle.** The square maps saturation to X and value to Y linearly, so arrow keys move in
  predictable steps and a click lands where expected. Blender's "Square (SV + H)" and Godot's HSV rectangle are this
  layout (R5.1, R5.6); Krita offers a triangle, which is harder to hit near its corners (R5.2, Judgment). The ring
  instead of a hue strip keeps the popover square and gives hue the longest track. Assistant's call.
- **Old and new chip**: left half shows the colour when the popover opened (click to restore), right half the current.
- **Fields**: Hex accepts `#RGB`, `#RRGGBB`, `RRGGBB` and CSS names ("teal"). RGB 0 to 255, HSV as 0 to 360, 0 to 100,
  0 to 100. Every field scrubs on horizontal drag over its label and takes expressions (UI doc 5.3). A "BrickColor"
  tab lists Roblox's named palette for imports and for creators who think in BrickColors (Unverified count; R2.7).
- **Recent**: the last 12 committed colours, per user, across projects (user config). Click applies.
- **Saved palettes**: per project, stored as `palettes/<name>.palette.ron` next to the scene files (diffable, shared
  with the team). "+" adds the current colour; drag to reorder; right-click Remove, Rename. A Theme's role colours
  (Look spec 1.5) appear as a read-only palette named after the Theme.
- **Harmony row**: from the current colour, generated in OKLCh (keep lightness and chroma, rotate hue) so the results
  look equally bright (Oklab's reason to exist: HSV value does not track perceived lightness, R5.8): Analogous
  (-30, -15, 0, +15, +30 degrees, Illustrator's offsets), Complementary (0, 180, and two lighter/darker
  steps), Split complementary (0, 150, 210), Triadic (0, 120, 240), Tints and shades (OKLab L at 0.25, 0.4, 0.55,
  0.7, 0.85). Out-of-gamut results are reduced in chroma until they fit sRGB. Click a chip to apply; Shift-click to
  add it to the saved palette.

### 11.2 Live preview and undo

- Opening the popover from the toolbar, Properties or C targets the selection.
- Dragging in the ring or square writes the colour to the **preview overlay** (2.1) for every target; the viewport
  shows it at once. Release commits one `SetProperties` step "Colour: #5280CC (3 parts)".
- Keyboard edits and typed fields preview immediately and commit when the user pauses for 600 ms, presses Enter, or
  focus leaves the popover; consecutive keyboard changes inside that window merge into one step.
- Esc closes the popover and drops any uncommitted preview; committed steps stay and Ctrl+Z undoes them.
- **Mixed selection**: the chip shows the active object's colour with a "Mixed" label. Dragging in the square sets
  every target to the same colour. Holding Alt while dragging the **ring** shifts each target's hue by the same delta
  and keeps its own saturation and value (recolour a whole painted house at once). Assistant's call.

### 11.3 Eyedropper

- I (or the popover's pipette) arms the eyedropper; the cursor becomes a pipette with a 24 px loupe showing the colour
  under it. I is the eyedropper key in Photoshop and Figma (R5.3, R5.4). With the paint flow below active,
  Shift+click samples, as in Bloxburg's paint tool (R3.8).
- Click a part: picks that part's **`Color` property**, not the lit pixel, because the lit pixel includes shading,
  fog and the Look (a red part in shadow would pick dark brown). Alt+click picks the rendered pixel instead.
- Ctrl+click picks colour, material, MaterialVariant and LookOverride together ("pick all"), then **paints** them onto
  the next parts clicked until Esc. Roblox's colour and material "click object to apply" modes, F3X's paint tool and
  Build a Boat's drag-to-paint are the models (R2.6, R2.7, R1.1, R3.9). Dragging across parts paints each one; one
  drag is one undo step.
- Clicking outside the viewport but inside Studio picks the UI pixel. Picking outside the Studio window needs an OS
  screen capture API: not in v1.

### 11.4 Keyboard and accessibility

- Tab order: ring, square, old/new, Hex, R, G, B, H, S, V, eyedropper, Recent, Palette, Harmony, close.
- Ring focused (WAI-ARIA slider keys, R5.7): arrows change hue by 1 degree, Shift+arrows by 10, Page Up/Down by 30,
  Home to 0 and End to 359.
- Square focused (React Aria's 2D colour area keys, R5.7): Left/Right saturation by 1, Up/Down value by 1,
  Shift+arrows by 10; Page Up/Down change value by 10; Home/End change saturation by 10.
- Swatch rows: arrows move, Enter applies, Delete removes (saved palette only).
- AccessKit: the ring is a slider ("Hue, 218 degrees"); the square is a 2D slider exposed as two values ("Saturation
  60 percent, value 80 percent"). After each change the label also gives the nearest colour name from a fixed list of
  148 CSS colour names in OKLab distance ("close to steel blue") so screen reader users hear something meaningful.
- Markers: the ring marker is a 16 px circle filled with the pure hue, a 3 px white border and a 1 px graphite outer
  ring; the square marker is a 12 px circle of the same build filled with the current colour. Both stay visible on any
  colour. The square is 146 px so its corners clear the ring's 106 px inner radius.
- Contrast: field text and labels use the visual-language tokens on `bg.raised`; the popover never puts text on the
  picked colour.

## 12. Material and per-part style

### 12.1 Material picker (M)

No mockup yet; the tiles reuse the style picker's tile build (12.5).

- Popover 560 x 520 px. Search field at the top ("Search materials, e.g. brick, rusty, pixel"). Filter chips:
  base material (the 10 of `MATERIALS`), set (Realistic, Stylized, Pixel, from the manifest's `look` field), and
  Favourites.
- Grid of 88 x 88 px tiles, name under each (two lines max). First row: Recent (last 8). Search matches name and the
  manifest `tags` (`assets/manifest/textures.toml`; 161 texture sets today). Results update per keystroke; ranked:
  exact name, name prefix, tag match.
- **Thumbnails render in the current Look**: a rounded cube lit by the Look's default lighting preset, 128 x 128,
  rendered once per (asset, Look) on first need and cached in the user cache folder as PNG. A tile shows a neutral
  grey placeholder with the name until its thumbnail is ready (the cube renders in under 2 ms each, section 17).
  Switching the place Look marks the cache stale; tiles re-render lazily as they scroll into view.
- Favourites: a star on hover; F toggles on the focused tile. Stored per user.
- Applying a library material: Studio creates (once) a `MaterialVariant` under `MaterialService` named after the
  asset, with `BaseMaterial` from the manifest's `material` field and its texture ids, then sets the parts' `Material`
  and `MaterialVariant`. One undo step "Material: Brick 076 (4 parts)". Applying one of the 10 base tiles clears
  `MaterialVariant`. Roblox names (R2.6); depends on #146 for the textures.
- Hover a tile 150 ms: the selection previews it (overlay). Arrow keys move, Enter applies, Esc closes. Typing
  starts the search. Roblox's 2024 Material Picker has hover preview, prefix search with Enter, and a paint mode
  (R2.6); this adds the CC0 library, favourites and Look-matched thumbnails.

### 12.2 `LookOverride`: the property

**Choice: a property on `BasePart` and on `Model`, not a child instance.** Assistant's call. Reasons:

| | Property | Child instance (`LookOverride` object) |
|---|---|---|
| Cost per part | 1 byte in the render instance, one string slot in the world (shared interned id) | One extra Instance per styled part: 5,000 styled parts = 5,000 more Explorer rows and world nodes |
| Multi-select edit | Properties already edits one property over many parts | Needs a custom "add child to each" flow |
| Copy and paste | Comes with Duplicate, Paste properties, the eyedropper's "pick all" | Must be copied as children; easy to lose |
| Scripts | `part.LookOverride = "retro-32"` | `Instance.new` plus parenting |
| Apply to a group | `Model.LookOverride`, inherited (below) | The only real advantage: it could sit under a Model; the Model property gives that |

- Type: string, same forms as `Lighting.Look` (Look spec 5.4): a Look id is stored; a display name is accepted on
  write and converted. Default `"inherit"`.
- **Allowed values in v1**:

| Value (id) | Shown as | Shading model taken from that Look | Per-part extras |
|---|---|---|---|
| `inherit` | Inherit (place Look name in muted text) | from the parent Model, else the place Look | |
| `realistic` | Realistic | Lit(wrap 0, floor 0) | |
| `soft` | Soft | Lit(wrap 0.3, floor 0.5) + its material rows | |
| `toon` | Toon | Toon(bands 2 ...) + Metal and Glass rows | hull outline 2 px (40,36,52) |
| `low-poly` | Low-poly | Flat(wrap 0.1) | |
| `voxel` | Voxel | Flat(wrap 0) | blocky detail at 8 texels per stud, nearest sampling |
| `pixel-3d` | Pixel 3D | Toon(bands 3, light_step_ev 1.0 ...) | nearest sampling; masked pixelate with k = round(window height / 270); masked edges 1 block; masked palette Auto 32, Bayer 4 |
| `retro-32` | Retro / PS1 | Retro(affine 1.0, vertex lighting, cutoff 0.5) | vertex snap to the k-pixel block grid; masked pixelate with k = round(window height / 240); nearest sampling; masked palette Levels 32, Bayer 4 |

  Comic, Ink, Painterly, HD-2D and the 2D Looks are not offered: what makes them recognisable is place-wide (halftone
  and hatch patterns, paper, Kuwahara, tilt-shift, sprite rendering), so per part they would look like Toon or
  Realistic and confuse people.
- **Resolution**: a part's effective style is its own value if not `inherit`; else the nearest ancestor Model's value
  that is not `inherit`; else the place Look. The renderer caches the resolved value per part and recomputes it for a
  subtree when a `LookOverride` or a parent changes (change journal).
- **Order against the Look spec 1.2**: an effective override replaces steps 1 to 3 of 1.2 with the override Look's
  own table: its `shading.materials[<Material>]`, then `shading.classes` (Character), then `shading.default`. A
  `MaterialVariant.ShadingModel` (Look spec 1.6, with #63) applies only when the part has no override. A Character
  part with an override uses the override (a PS1 dummy in a realistic world is the point).
- Saved in the scene file like any property; omitted when `inherit` (scene files stay small).

### 12.3 What can vary per part, and what cannot

| Look field | Per part? | How | Why |
|---|---|---|---|
| `shading` (model and parameters, material rows) | **Yes** | GPU material table row per (style, material) | Chosen per draw already (#63) |
| `outlines.hull` | **Yes** | Hull pass draws only instances with the hull flag, width and colour from their row | The hull is per mesh by nature |
| Retro vertex snap | **Yes** | Vertex shader snaps screen xy to multiples of the style's block size k (4 px at 1080p) when the flag is set | Per vertex; no screen pass |
| Affine UVs | **Yes** | Vertex outputs both perspective and linear UVs; fragment blends by the row's `affine` | Same as `ps1_retro.wgsl` |
| Nearest sampling, `detail_texels_per_stud` | **Yes** | Both samplers bound; branch on a row flag | Per draw |
| `palette` quantise | **Masked** | Style id target; the palette pass quantises a pixel with that style's palette parameters | Must run after the tonemap, so it is a screen pass; a mask keeps it per part |
| `outlines.edges` | **Masked** | Edge pass draws a line pixel only when either side's style asks for edges | Screen pass; edges between a styled part and others still draw on the styled side |
| `PixelHeight` (chunky pixels) | **Masked, approximated** | Pixelate pass: a k x k screen block whose centre pixel has a pixelating style takes that centre pixel's colour; a block whose centre does not, keeps its own pixels. The styled part's silhouette therefore becomes blocky too | True low-resolution geometry would need a second scene pass; this costs one full-screen pass with two texture reads per pixel |
| The place's render scale, upscale filter, sub-texel shift | **No** | | One internal image per frame |
| Tonemap, exposure, `grade` | **No** | | Per-part curves make the same white look different on neighbouring parts and read as a bug |
| Lighting preset, shadows, AO, ambient, volumetrics, fog | **No** | | Light is shared by the scene; a part cannot have its own sun without breaking its shadows on neighbours |
| `hdr` (bloom, DoF, Kuwahara), `ldr` (patterns, paper), `output` (CRT), AA | **No** | | They blur or pattern across part borders; masking them leaves hard seams |

What a creator sees: a Retro part in a Realistic place has chunky 4 px pixels (at 1080p), wobbling vertices, warped
nearest-filtered textures, per-vertex light and 15-bit colour banding with dither, while everything around it stays
sharp. The thumbnails in the style picker mockup are rendered with exactly these rules. Limits the creator should know
about, stated in one line under the tiles: "Lighting, bloom and fog stay with the place Look." Known artefact: where
a sharp object passes in front of a pixelated one, the boundary can be blocky by one block (start value; accepted
because the pixel styles already shimmer under perspective cameras, Look spec 3.5).

A place whose Look is itself a palette or pixel Look keeps its global passes for `inherit` pixels. An override to
`realistic` inside a Retro place skips the palette for its pixels but still renders at 240 lines.

### 12.4 Renderer

Builds on #63 (shading model id and GPU material table):

- **Material table rows**: one row per (resolved style, `Material`) pair in use, plus the class rows. The place Look
  fills rows 0..N; each override style present in the world adds its rows on first use. At most 8 styles x (10
  materials + 2 classes) = 96 rows, 64 bytes each: 6 KB. Rows are rebuilt only when a Look file or the set of used
  styles changes.
- **Instance data**: `InstanceRaw.material` becomes four `u32` (bitcast in WGSL): [0] material table row, [1] flags
  (bit 0 hull, bit 1 vertex snap, bit 2 affine, bit 3 nearest, bits 4..7 style slot 0..15), [2] the material id for
  procedural detail as today, [3] reserved. #63 plans the model id in slot 3; this spec moves it into the row (the
  row holds the model id), so per-instance data stays 16 bytes. This is a change request to #63, filed as a comment.
- **Pipelines**: no new surface pipelines. The uber shader already switches on the model id (Look spec, #63). The
  hull pass gets an instance list of flagged instances. Vertex snap and affine run in the shared vertex shader behind
  the flag (a uniform branch per instance).
- **Style id target**: an `R8Uint` target written by the opaque pass with the style slot, allocated only while at
  least one resolved style uses a masked effect that differs from the place Look. At 1080p internal size: 2 MB. The
  pixelate, palette and edge passes read it; with no mask target they run as today.
- **Pixelate pass** (new, masked): runs after AO apply (Look spec 1.3 step 6) and before the HDR list, on the HDR
  image. For each pixel: read the style slot at its k x k block centre; if that slot pixelates, output the centre
  pixel's colour and write the slot into the mask for later passes; else keep the pixel. k comes from a 16-entry
  uniform array by slot. For Pixel 3D the edge pass then compares depth and normal **between block centres**, so lines
  are one block thick, like the global Pixel 3D Look. Block grid anchored at the window's top-left pixel.
- **Frame order with overrides**: shadows, prepass, opaque + hull (writes style slot), sky and transparent, edges
  (masked), AO apply, **pixelate (masked)**, HDR list, finish, LDR list, **palette (masked)**, upscale, output, UI.
- **Palette pass**: a uniform array of up to 16 palette descriptors (mode, levels or palette offset, dither,
  strength) indexed by the style slot; slot 0 is the place Look. `Auto` palettes for overrides are built from the
  colours of parts with that style only.
- **Transparent parts** do not write the style id; they keep the place Look's masked effects. Start value; revisit if
  a PS1 window looks wrong.
- Cost targets: section 17.

### 12.5 Style picker UI (L)

Mockup: `style-picker.html`.

- **Where**: the toolbar's style chip (L), and a **Style** row in Properties > Appearance under Material: a 24 px row
  showing the current value with a small thumbnail ("Inherit · Realistic"). Ctrl K lists "Style: Retro / PS1" for
  the selection.
- **Popover** (480 x 360 px): title "Style for 1 model (24 parts)"; a 4 x 2 grid of tiles 104 x 78 px plus the name;
  the `inherit` tile first, labelled "Place Look (Soft)" in a place whose Look is Soft. Under the grid: the one-line
  note from 12.3 and a "Place Look..." link that opens the Look picker (Look spec 5.1).
- **Live thumbnails**: each tile renders **the current selection** (framed to fill the tile, three-quarter view, the
  place's lighting) in that style at 208 x 156 (2x) when the popover opens. Thumbnails render one per frame so the
  popover opens instantly; until a tile's image is ready it shows the static reference-scene thumbnail from the Look
  spec 5.1 with "Rendering" in muted text. Pipelines come from the background precompile (#73). Pixel styles use a
  block of 3 thumbnail pixels so they read at tile size; the hover preview in the viewport shows the true block size.
- **Hover** 150 ms: the viewport previews the style on the selection (overlay); leaving restores it.
- **Click** applies: one undo step "Style: Retro / PS1 (1 model)". When the selection is a single Model, the value
  is written to the Model (children keep `inherit`); when it is parts, to each part. A footer segment "Apply to:
  Model / Each part" switches this (Assistant's call).
- **Visible state**: a part or model with an override shows a 12 px style glyph in its Explorer row's meta column
  (and the style name on hover). View > Shading > "Style overrides" tints each style slot in a flat debug colour with
  a legend, so a creator can find stray PS1 parts.
- **Keyboard**: arrows move between tiles, Enter applies, Space previews without applying, Esc closes. AccessKit:
  radio group "Style", each tile a radio button with name and description ("Retro / PS1: wobbly vertices, warped
  textures, 15-bit colour").

## 13. Building in Play

### 13.1 Studio: edit during Play

- **F4 toggles Build in Play** while Play runs (Metallic preset). The game keeps running; the mouse is released from
  the game; the viewport camera detaches from the character (fly camera, as in edit); the tools, gizmo, contextual
  toolbar and colour popover work on the **play copy**.
- A part being dragged has its physics paused (kinematic) until release, then returns to its previous `Anchored`
  state. Scripts see the change as a normal property change.
- The green Play markers (UI doc decision 4) stay; the viewport header shows "Building in Play: changes are not saved
  unless you keep them".
- On Stop, the changes appear in #130's Keep/Discard list grouped as "Built during Play (14 changes)". Keeping applies
  them to the edit world as one undo step. #130 owns that list; this spec only adds the group.
- F4 again (or Esc with nothing selected) hands the mouse back to the game.

### 13.2 Runtime build kit for players

Game creators give players building in their own games (Bloxburg's build mode, Build a Boat's tools, the Build
Sandbox template). Two parts:

**Engine classes (Roblox names, R2.11):**

| Class | What it does | Notes |
|---|---|---|
| `Handles` | Six arrow handles on an `Adornee`; `MouseDrag(face, distance)` | Same names as Roblox |
| `ArcHandles` | Three rotation rings; `MouseDrag(axis, relativeAngle, deltaRadius)` | Same names as Roblox |
| `DragDetector` | Makes a part draggable by players: `DragStyle`, `ResponseStyle` (Geometric anchors the part while dragged), `RunLocally` (false: the server performs the drag), `AddConstraintFunction` (for example grid snapping), events `DragStart`, `DragContinue`, `DragEnd` | Same names as Roblox (R2.11); v1 supports the styles TranslateLine, TranslatePlane, RotateAxis and BestForDevice, and the Geometric and Custom responses |

**`BuildKit` package (Luau, MIT, shipped in Assets > Packages)**, generalising `docs/templates/build-sandbox.md`:

- `BuildKit.new(config)` on the server with: `Catalog` (Folder of block Models), `GridSize` (default 1), `RotationStep`
  (90 or 45), `Plots` (tag name) or `Region` (bounds), `MaxBlocks`, `AllowedMaterials`, `Palette` (list of Color3 or
  `"any"`), `CanPlace(player, cframe, block)` callback, `SaveKey` (DataStore key or nil).
- Client: ghost placement with the same surface and grid rules as section 5.2 (shared `Grid` module). Default keys
  follow what players already know from Roblox build games: R rotate and T tilt (Build a Boat, the Build Sandbox
  doc), X delete, F paint and C clone (Bloxburg), H grid on/off and J grid size (Bloxburg), Shift+click with paint
  samples a colour (Bloxburg), Ctrl+Z undo, 1 to 9 for the catalog (R3.8, R3.9). All are #180 input actions, so
  gamepad, touch and player rebinding come with them. Ghost outline white when allowed; red outline **plus** a cross
  icon when not.
- `Tools` config: the list of tools offered (place, move, rotate, scale, paint, clone, delete, mirror) and a
  `CanUseTool(player, tool)` callback, so a game can sell or unlock tools as progression (Build a Boat sells its
  scaling, paint and trowel tools for in-game gold, R3.9).
- Export: `BuildKit:ExportPlot(player)` returns the plot as a scene-file fragment the creator can open in Studio.
  F3X's export depended on a third-party server that is now down (R1.2); Metallic keeps the format in the platform.
- Colour for players: a smaller version of section 11's popover drawn with the in-game UI (ring, square, Hex, recent
  8, the game's palette), no eyedropper. Creators can lock it to their palette.
- Server: every request validated (catalog, snap, region, overlap, limits, rate 10 per second), undo 50 per player,
  save on interval and leave. The Build Sandbox's remote table (template doc) is the protocol.
- Styles for players: `AllowedStyles` (default none). When set, players can pick a `LookOverride` from that list for
  their blocks.

## 14. Discoverability

### 14.1 First-time hints

- One hint card at a time, bottom left of the viewport above the Litha strip (UI doc 5.5), 320 px wide, with a
  keycap, one sentence, "Got it" and "Show me" (a 4 s looping clip or an animated overlay on the real selection).
- Triggers are behaviours, not timers. Each hint fires at most once per user:

| Id | When | Text |
|---|---|---|
| `first-select` | First part selected | "Drag to move it. W, E, R switch to move, rotate and scale." |
| `drag-snap` | First body drag ends off-grid with snapping off | "Hold Ctrl while dragging to snap to the 1-stud grid." |
| `type-value` | Third gizmo drag on the same axis within 30 s | "While dragging, type a number for an exact move. Try 5 then Enter." |
| `smart-dup` | Second Ctrl+D followed by a move of the copy | "Press Ctrl+D again to repeat that offset." |
| `align` | Three parts moved one by one to the same Y within 60 s | "Select them all and press A to line them up in one go." |
| `vertex-snap` | A drag ends within 0.2 studs of another part's corner | "Hold V to snap corner to corner exactly." |
| `colour-hue` | Colour changed on 3+ parts one at a time | "Select them together; Alt-drag the hue ring to recolour and keep each shade." |
| `style` | First time the Look picker is used | "You can also give one object its own style: select it and press L." |

- Rules: never during a drag, Play or camera flight; at most one hint per 3 minutes; Esc dismisses the card while
  the pointer is on it or one of its buttons has focus (elsewhere Esc keeps its editor meaning: clear the selection,
  cancel a drag), and "Got it" always does; Settings > "Show tips" turns all off; "Reset tips" brings them back.
  State lives in user config `hints.ron`; an unreadable file is logged, moved aside to `hints.ron.bad` and the
  hints start fresh.

### 14.2 Finding everything

- Every tool and setting is a Ctrl K command ("Align: left (min X)", "Array: radial", "Snap: vertex").
- Tooltips (500 ms, UI doc 5.12) give name, one sentence and the key.
- Ctrl+/ shows all shortcuts with the building ones in their own group.

### 14.3 Accessibility checklist for every building surface

- Every action reachable without a mouse: the selection can be moved with the keyboard (Metallic preset: Alt+arrow
  keys nudge by the move increment on X and Z relative to the camera, Alt+Page Up/Down on Y; Shift for 10x). Rotate:
  Alt+, and Alt+. by the rotate increment. F3X and Roblox Studio both nudge with the numpad (R1.1, R2.1).
- Gizmo handles expose AccessKit roles; the viewport exposes the selection list. "Transform by..." (Ctrl+Shift+T)
  opens a small dialog with Move X/Y/Z, Rotate X/Y/Z and Scale fields for exact edits without a drag; Enter applies
  one undo step.
- Colour never alone: axis letters, snap glyph shapes, ghost cross icons, "Mixed" labels.
- Focus ring 2 px `focus` on every popover control; popovers trap focus until Esc.
- Reduced motion: the toolbar fade and hint card motion become instant.
- High contrast theme (#134): the toolbar and popovers get `line.hc` borders.

## 15. Keymap defaults

Rows #129 must hold, in its three presets. Keys under Roblox Studio and Unity come from the research (R2, R4.1); a
blank cell means that tool has no default and the Metallic key is used.

Context "drag" means while a body or gizmo drag is held; those keys mean nothing else during a drag.

| Action | Metallic | Roblox Studio preset | Unity-style preset |
|---|---|---|---|
| Select, Move, Rotate, Scale tool | Q, W, E, R (UI doc 5.15) | 1, 2, 4, 3: Select 1, Move 2, Scale 3, Rotate 4 (R2.1) | Q is View in Unity, so Select is Esc then click; W, E, R (R4.1) |
| Transform tool (all handles) | T | none (Roblox's Transform tool key is unverified, R2.1) | Y (R4.1) |
| Cycle select mode (box, lasso, paint) | Q again | 1 again | header only |
| Snapping inverted while dragging | Ctrl (UI doc 5.5) | Shift (R2.1) | Ctrl (R4.1) |
| Precision drag (one tenth) | Shift (UI doc 5.5) | not bound (Shift is snapping) | not bound |
| Grid snap on/off | header chip, Ctrl K | Shift+X (R2.2) | header chip |
| Snap to parts on/off | header chip | Shift+C (R2.2) | header chip |
| Halve, double move increment | `[`, `]` | Shift+2 focuses the field (R2.2); `[`, `]` also bound | Ctrl+`[`, Ctrl+`]` (R4.1) |
| Rotate increment down, up | Shift+`[`, Shift+`]` | Alt+R focuses the field (R2.2) | Shift+`[`, Shift+`]` |
| World / Local | X | Ctrl+L (R2.1) | X (Unverified, R4.1) |
| Pivot / Centre | Z | not bound | Z (Unverified, R4.1) |
| Edit pivot | P | P (Roblox has a button only, R2.3) | P |
| Vertex snap (hold) | V | V (Roblox soft-snaps without a key, R2.2) | V (R4.1) |
| Vertex onto surface (hold) | V+Shift | V+Shift | V+Shift+Ctrl (R4.1) |
| Axis lock (drag) | X, Y, Z; Shift+X/Y/Z for planes (R4.9) | same | same |
| Duplicate and move | Alt+drag (R4.4) | Ctrl+D during the drag (R2.1) | Alt+drag |
| Stamp a copy (drag) | Ctrl+D (R2.1) | Ctrl+D | Ctrl+D |
| Rotate 90, tilt 90 (drag) | R, T (R2.1) | R, T | R, T |
| Rotate 90, tilt 90 (selection) | Ctrl+R, Ctrl+T (R2.1) | Ctrl+R, Ctrl+T | not bound |
| Drop onto the surface below | End (R4.4) | End | End |
| Bring the gizmo to the cursor (hold) | Space | Tab (R2.1) | Space |
| Scale: both faces, uniform, about pivot | Alt; corner handle; Pivot mode | Ctrl; Alt (R2.1); Ctrl is also about pivot in Roblox, so both faces is not bound | Alt; centre cube; Pivot mode |
| Snap selection to grid | Ctrl+Shift+Home | Ctrl+Shift+Home | Ctrl+Shift+Home |
| Transform by... dialog | Ctrl+Shift+T | Ctrl+Shift+T | Ctrl+Shift+T |
| Nudge by one increment | Alt+arrows, Alt+Page Up/Down | Numpad keys (R2.1) | Alt+arrows |
| Rotate by one increment | Alt+`,`, Alt+`.` | Alt+`,`, Alt+`.` | Alt+`,`, Alt+`.` |
| Align popover | A | A | A |
| Mirror in place, mirror copy | Ctrl+M, Ctrl+Shift+M (R4.9) | same | same |
| Array popover | Ctrl+Shift+D | same | same |
| Brush | B | B | B |
| Measure | Shift+M | Shift+M | Shift+M |
| Select similar | Shift+G (R4.9) | Shift+G | Shift+G |
| Colour, material, style popovers | C, M, L | C, M, L | C, M, L |
| Eyedropper | I (R5.3, R5.4) | I | I |
| Focus the contextual toolbar | Ctrl+. | Ctrl+. | Ctrl+. |
| Group, ungroup | Ctrl+G, Ctrl+Shift+G (UI doc) | Ctrl+G, Ctrl+U; group as Folder Alt+Ctrl+G (R2.9) | Ctrl+G, Ctrl+Shift+G |
| Anchor, lock | Alt+A, Alt+L (Roblox keys, R2.8) | Alt+A, Alt+L | Alt+A, Alt+L |
| Union, negate, intersect, separate (reserved for CSG) | Ctrl+Alt+G, Ctrl+Shift+N, Ctrl+Shift+I, Ctrl+Alt+Shift+G | Ctrl+Shift+G, N, I, U (R2.5) | as Metallic |
| Build in Play | F4 | F4 | F4 |

Rules: no binding uses a key the UI doc 5.15 already assigns in the same context. Viewport-context keys (single
letters) do nothing while a text field has focus. The Metallic preset uses Space, not Tab, to summon the gizmo
because Tab moves keyboard focus between controls (UI doc 5.12) and taking it in the viewport would trap keyboard
users. The Roblox preset keeps Tab for muscle memory; F6 still leaves the viewport. Unity keys marked Unverified
must be checked against Unity's current shortcut manager when #129 builds the preset.

## 16. Luau API

Roblox names where Roblox has the feature; Metallic additions marked +. These are run-time APIs (scripts and the
build kit); Studio's own tools use the same functions internally where they exist.

```lua
-- Pivots (PVInstance: BasePart and Model)
local cf = model:GetPivot()                 -- CFrame of the pivot in world space
model:PivotTo(CFrame.new(0, 5, 0))          -- moves the model so its pivot lands there
part.PivotOffset = CFrame.new(0, -1, 0)     -- pivot relative to the part
model.WorldPivot = CFrame.new(10, 0, 0)     -- pivot of a model, world space

-- Size and scale
local ok = part:Resize(Enum.NormalId.Top, 2) -- grows the top face by 2 studs; false if it would collide (Roblox rule)
model:ScaleTo(1.5); print(model:GetScale())
local boxCf, size = model:GetBoundingBox()
local extents = model:GetExtentsSize()

-- Queries used by placement code
local hits = workspace:GetPartBoundsInBox(cf, size, overlapParams)
local result = workspace:Raycast(origin, direction, raycastParams)

-- Look
part.LookOverride = "retro-32"              -- + also accepts "Retro / PS1"; "inherit" clears
model.LookOverride = "toon"                 -- + applies to descendants that inherit
print(part:GetEffectiveLook())              -- + "retro-32", after inheritance

-- Material
part.Material = Enum.Material.Brick
part.MaterialVariant = "Brick 076"          -- name of a MaterialVariant under MaterialService

-- Runtime building (engine classes, Roblox names)
local h = Instance.new("Handles"); h.Adornee = part; h.Style = Enum.HandlesStyle.Resize
h.MouseDrag:Connect(function(face, distance) end)
local d = Instance.new("DragDetector"); d.DragStyle = Enum.DragDetectorDragStyle.TranslatePlane; d.Parent = part

-- Tags (from #78)
part:AddTag("Brush:Rocks"); for _, p in CollectionService:GetTagged("Brush:Rocks") do end
```

- `Material` is a string in `metallic_core`; Luau accepts `Enum.Material.X` and the string, and returns the enum, as
  Roblox does (#469).
- `BasePart:Resize` returns false and changes nothing when the result would overlap a part with `CanCollide` (Roblox
  behaviour, R2.11, to verify against the reference while implementing).
- `GetEffectiveLook` is new; Roblox has no per-part style.
- Studio-side APIs for future plugins (`Selection`, `ChangeHistoryService`) are not in scope; when they come, they use
  Roblox names (R2.11).

## 17. Performance budgets

Reference PC as in the UI doc; "Iris Xe" is the Low tier laptop. Numbers are start values; the issues' tests check
them on the reference PC with release builds.

| Operation | Scene | Budget |
|---|---|---|
| Gizmo hover hit test | any | 0.05 ms per frame (CPU) |
| Surface snap ray | 50,000 parts | 0.2 ms per frame (BVH from the picker) |
| Vertex/edge snap candidates in 12 px | 50,000 parts, 200 on screen near the cursor | 0.5 ms per frame |
| Body drag of a selection | 1,000 parts | 4 ms per frame for overlay updates; 60 fps viewport |
| Commit of a drag | 1,000 parts | one `SetProperties` command, under 10 ms |
| Box or lasso select | 10,000 parts on screen | 5 ms (box), 10 ms (lasso) |
| Select similar | 50,000 parts | 20 ms |
| Align, distribute, mirror | 1,000 parts | 5 ms |
| Array preview | 200 ghosts | 2 ms CPU; ghosts drawn as instanced silhouettes |
| Brush stroke | 16-stud radius, density 2 | 1 ms per placement batch; at most 64 placements per frame |
| Colour live preview | 5,000 selected parts | visible next frame (overlay; per-instance colour upload only for changed instances) |
| Material thumbnail render | 128 x 128 cube | 2 ms GPU each; at most 4 per frame while the picker is open |
| Style thumbnails | 8 tiles at 208 x 156 | one per frame, 3 ms GPU each, after pipelines exist |
| Per-part style, renderer | 5,000 parts, 3 styles, Low tier, 1080p | +0.3 ms GPU over the place Look alone (style target write + masked palette); +0.3 ms more when a pixelating style is on screen (one full-screen pass, two reads per pixel); no added draw calls except the hull list. Judgment from the Look spec's per-pass costs (3.6); measure on Iris Xe |
| `LookOverride` resolve | change on a Model with 5,000 descendants | 1 ms CPU |
| Memory | per part | +1 byte resolved style in render data; property string interned |

The Studio idle rule (#124) holds: none of these tools repaints while the user does nothing.

## 18. Mockups

`docs/design/building-tools/`, self-contained HTML: Archivo and JetBrains Mono subsets embedded from
`docs/design/visual-language/assets/mockup-fonts.css`, tokens copied from `tokens.css`, icons drawn as inline SVG,
viewport pictures embedded as JPEG. A PNG of each sits next to it. The viewport pictures and style thumbnails are
stand-ins from a small numpy rasteriser written for the mockups (not engine code); it applies the per-part rules of
12.3 (vertex snap, affine UVs, block pixelate, 15-bit dither, palette, block edges), so the thumbnails show what the
spec describes.

| File | Shows |
|---|---|
| `viewport-toolbar.html` / `.png` | Viewport header with the building settings (tools with keys, increments, World/Pivot, snap chips, Collisions); two selected walls; the move gizmo at the active wall's pivot; the contextual toolbar 12 px above the selection; the editable "Moved X +4.00" field after a move (4.5); one hint card |
| `colour-popover.html` / `.png` | Two walls previewing #5280CC; the colour popover opened from the toolbar's swatch: ring and square with keyboard focus on the square, old/new chip, Hex/RGB/HSV, recent, the "Castle" palette, an analogous harmony row computed in OKLCh |
| `style-picker.html` / `.png` | The Props model hover-previewing Retro / PS1 in a Soft place (only the crate, barrel and ball turn chunky and dithered); the style popover with eight live tiles, the place-wide note, the Apply to segment; the Style row in Properties |

## 19. Assistant's calls (overturn any)

1. Increments stored per place, not per install (2.2).
2. X, Y, Z lock axes during a drag and toggle World/Local and Pivot outside one (4.1, 15).
3. Typed values during a drag (4.5).
4. Ring and square colour picker, not triangle (11.1).
5. Alt-drag on the hue ring keeps each part's shade (11.2).
6. Eyedropper reads the property, Alt for the rendered pixel (11.3).
7. `LookOverride` as a property on BasePart and Model, inherited from the nearest Model (12.2).
8. Eight allowed styles; Comic, Ink, Painterly, HD-2D and 2D Looks excluded (12.2).
9. Style written to the Model when one Model is selected (12.5).
10. Transparent parts keep the place's masked effects (12.4); per-part pixelate by block-centre sampling (12.3).
11. F4 for Build in Play (13.1); Space to summon the gizmo in the Metallic preset (15).
12. `BuildKit` as a Luau package rather than an engine service (13.2): creators can read and change it, and it is the
    same code the Build Sandbox template teaches.
13. CSG waits for the modeller (1).
14. Palettes stored as project files (11.1).

## 20. Open questions

- Should a Model's `LookOverride` also apply to its non-Part descendants that draw (particles, decals)? This spec
  says no: particles follow the place Look (Look spec 1.2).
- Masked palette with `Auto` per style needs the Look spec's unproven k-means builder (Look spec 8).
- Whether Roblox's `Handles` and `DragDetector` events match the behaviour described here needs checking against the
  live API while implementing (research R2.11 marks the gaps).
