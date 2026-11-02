# #469 enums as registry items (step 12 of the class-system migration)

Branch `studio/469-enums`. Design: `docs/design/class-system.md` (sections 9, 11, 14 row #469, 17 question 2).

## Goal
Enumerated property values become registry enums exposed as `Enum.<Name>.<Item>`. Enum properties read as `EnumItem` (the one
intended script-visible change), writes take the item or its name, and core keeps storing the name.

## Done
- `EnumDesc` in `classes/desc.rs`; `ClassDesc::enums`; `pub static ENUMS` in `classes/enums/*.rs`, found by `build.rs`.
- `ResolvedEnum`, `EnumId`, `Registry::enums`, `enum_by_name`, `resolve_with_enums`, seven new `SchemaError` variants.
- Enums declared: `Material`, `PartType` (`base_part.rs`; the property is still `Shape`); `InputActionType`, `InputTrigger`, `InputRole`, `TouchSlot`,
  `TouchLayout` (`input_action.rs`); `CameraType` (`camera.rs`); `LightingStyle` (`lighting.rs`); `TonemapperPreset`
  (`color_grading_effect.rs`); `KeyCode`, `UserInputType`, `UserInputState`, `ContextActionResult`, `ContextActionPriority`,
  `PreferredInput`, `RotationOrder`, `NormalId` (`classes/enums/`).
- `registry_hash` covers enums (still `metallic-registry-v5`). Who declares an enum and its description are not hashed.
- `metallic_script/src/api/enums.rs` holds `LuaEnumItem`, the `Enum` global, the read and the write; moved out of `input.rs`.
- Connected to #461: `EnumDesc::item_ref`, `PropDesc::script_value`, `PropDesc::stored_item`; `LuaEnumItem` and `Wire::EnumItem`
  hold an `EnumItemRef`.
- Tests: `crates/metallic_core/tests/enums.rs`, `crates/metallic_script/tests/enums.rs`, unit tests in `api/enums.rs` and
  `replication.rs`.

## Next
Nothing in this issue beyond review. Merged with #462 (generic binding): an enum read is `HostApi::get_property` giving
`ScriptValue::EnumItem` through `PropDesc::script_value`, and `script_to_lua` gives this VM's item object; `set_property` takes an
item through `PropDesc::stored_item`; `set_member` uses `enums::enum_write` so a Luau write keeps its error texts. `api/instance.rs`
has that one match and no other enum code, and `crates/metallic_script/tests/enums.rs` fails if a read goes back to a string.
`Ty::Enum` still refuses a plain string (#461's decision, tested): a method that should also take a name declares `.manual()`.

## Decisions where the design left room
- `part.Material == "Metal"` is false with no error: Luau does not call `__eq` across types, and Roblox behaves the same.
- No `__concat` on `EnumItem`: as in Roblox, and a loud error beats a silently different string.
- `EnumDesc::items` is `&[&str]` plus optional `numbers`, as #469's text says, instead of the design's
  `&[EnumItemDesc { name, value }]`: `allowed` is then the same slice, with nothing to allocate and `World::set` untouched.
  Section 9 is updated.
- `PropDesc::enum_type` is `Option<&'static EnumDesc>`, not a name: `enum_of` and `stored_form` need no registry lookup, and the
  resolution check refuses a property whose enum is not the one declared under that name.
- `EnumItemRef` stays name-based instead of the design's `{ enum: EnumId, item: u16 }`. `new` is `const` and takes names, and
  `tests/call_convention.rs` uses it with no registry and must stay unedited; ids would also differ between fixture registries.
  Section 5.1's comment is updated.
- `aliases` are `(old item name, item)`. No built-in enum has one yet, so alias tests use fixture registries rather than an
  invented alias. `World::set` reaches them through `PropDesc::stored_form`, only after a value failed the `allowed` check.
- A remote argument can be an `EnumItem` (`Wire::EnumItem`). Without it `FireServer(part.Material)`, which worked with a string,
  would have broken. Attributes refuse an item: they have no item type and core's `Value` should not gain one for this.
- `InputBinding.KeyCode` stays a plain String: it names runtime controls, its default `"None"` is not an `Enum.KeyCode` item, and
  it has no `allowed` list. `lua_to_value` keeps the rule that lets it take `Enum.KeyCode.X`.
- `LookOverride` stays a String: not in #469's list, and it has a display-name normalizer.
- `MATERIALS` remains as `pub const` derived from `MATERIAL.items`, because `metallic_render`'s
  `material_table_has_every_registry_material` uses it and may not be edited. `KEY_CODES` and `USER_INPUT_TYPES` remain in
  `metallic_script` the same way, for the `metallic_runtime` test. No hand list is left.
- `Value` numbers are positions except `ContextActionPriority`. Roblox's numbers for `Enum.Material` were not added: not asked
  for, and not checkable here.
- A `Ty::Enum` that names no enum is checked by a test over the built-in classes, not in `resolve`: `call_convention.rs` fixtures
  name `Material` and `NormalId` without declaring them.

## Edited tests, and why
- `crates/metallic_core/tests/replication.rs` `registry_hash_is_pinned`: new `GOLDEN`, because the hash now covers enums.
- `crates/metallic_core/src/replication.rs` `fingerprint_covers_property_metadata_defaults_order_and_class_identity`: new pinned
  bytes, because the stream gained the enum count. Recomputed with `printf | sha256sum`: the old stream gives the old bytes, and
  the old stream plus eight zero bytes gives the new ones.
- `crates/metallic_script/tests/generic_binding.rs` `CLASS_LITERALS_LEFT`: `Camera` 3 to 2, because the `CameraType` arm is gone.
  That table counts those arms, and the merge with #462 is where the two met.

No test asserted that an enum property reads as a string, and no other test needed an edit.

## Scripts checked
`git grep` over the whole repo, plus the `Source` strings in every `templates/**/*.mscene`, for reads of `.Material`, `.Shape`,
`.CameraType`, `.LightingStyle`, `.TonemapperPreset`, `.Type`, `.Trigger`, `.Role`, `.TouchSlot`, `.TouchLayout` and the `["..."]`
forms: no template script reads one, and the only reads in Rust test scripts already compare with the item
(`metallic_runtime/tests/camera_api.rs`). No script needed updating.

## Perf
Windows 11, a working desktop with other sessions active, not a controlled machine. Base `c007d15`, head `e402009`, `--release`,
run interleaved. The CI gate counts Callgrind instructions, which needs Linux and Valgrind: **not run here**.

`perf_ci`, `elapsed_ns` (61 samples for `reads` and `writes`, 31 for the rest):

| Workload | Base min / median | Head min / median | Median change |
|---|---|---|---|
| `reads` | 2.371 / 2.625 ms | 2.337 / 2.524 ms | -3.8% |
| `writes` | 1.014 / 1.114 ms | 0.992 / 1.091 ms | -2.1% |
| `clone` | 1.313 / 1.656 ms | 1.310 / 1.633 ms | -1.4% |
| `create` | 0.085 / 0.092 ms | 0.083 / 0.099 ms | +7.2% |
| `destroy` | 0.506 / 0.575 ms | 0.514 / 0.582 ms | +1.2% |

An earlier run of 31 samples gave `reads` +10.2% and `writes` +15.8% at the median with head's minimum still lower; the 61-sample
rerun above did not repeat it, so that run is read as machine load. `create` runs no changed code: `World::create` is untouched.
It may be layout or noise; that is inferred, not traced, so it is the one to watch at the gate.

`perf_ci_lua`, 15 samples:

| Workload | Base min / median | Head min / median | Median change |
|---|---|---|---|
| `lua_reads` | 22.839 / 25.421 ms | 22.876 / 24.848 ms | -2.3% |
| `lua_writes` | 15.528 / 16.381 ms | 15.409 / 16.385 ms | 0.0% |
| `lua_method_calls` | 36.134 / 36.751 ms | 35.085 / 35.781 ms | -2.6% |

Those workloads read no enum property. A throwaway probe (1,000,000 operations per sample, 15 samples, two runs, not committed)
measured the enum paths; numbers are ns per operation including the loop, base median then head median per run:

| Operation | Base | Head |
|---|---|---|
| `x = p.Material` | 951, 949 (string) | 821, 731 (item) |
| `x = p.Material == 'Metal'` then `== Enum.Material.Metal` | 994, 1012 | 1080, 1058 |
| `p.Material = 'Metal'` | 917, 959 | 972, 959 |
| `p.Material = Enum.Material.Metal` | not comparable | 1131, 1124 |
| `x = p.Anchored` (control) | 662, 703 | 718, 679 |
| `x = p.Name` (control) | 1014, 1012 | 1044, 999 |

An enum read is cheaper than the string read was (about 14% to 23%): it clones a cached item and allocates no string. The compare
costs about 5% to 9% more than the string compare did, and that line also looks up `Enum` and `Material`. `.Name` on the result
is a further userdata field read: `p.Material.Name` measured 1383 and 1368 ns.

### After the merge with #462
Base `d5d6657` (main), head `0a8f9d2`, same kind of machine and method, with no artificial load running. `Ir` still not measured.

| Workload | Base min / median | Head min / median | Median change |
|---|---|---|---|
| `lua_reads` | 20.948 / 22.792 ms | 20.877 / 22.864 ms | +0.3% |
| `lua_writes` | 15.210 / 16.614 ms, 15.060 / 15.447 ms | 15.012 / 16.467 ms, 14.858 / 15.314 ms | -0.9%, -0.9% |
| `lua_method_calls` | 25.949 / 27.651 ms | 25.753 / 27.988 ms | +1.2% |
| `reads` | 2.458 / 2.688 ms | 2.303 / 2.646 ms | -1.6% |
| `writes` | 1.004 / 1.060 ms | 0.971 / 1.051 ms | -0.9% |
| `clone` | 1.283 / 1.717 ms | 1.451 / 1.726 ms | +0.6% |
| `create` | 0.084 / 0.090 ms | 0.085 / 0.091 ms | +0.4% |
| `destroy` | 0.430 / 0.488 ms | 0.424 / 0.490 ms | +0.4% |

31 samples each, except `lua_writes`: two runs of 61. A first 31-sample run of `lua_writes` gave +4.4% at the median with the
minimum at -0.3%; the two longer runs did not repeat it. The `lua_*` workloads read no enum property. The enum-read probe in the
section above was measured before #462 and was not repeated.

## Commands
```
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p metallic_core -p metallic_script -j 4 --no-fail-fast
cargo run -p metallic_core --example registry_hash    # new golden value after an enum or class change
find crates/metallic_core/src/classes -mindepth 2 -name '*.rs' -print0 | xargs -0 rustfmt --edition 2024 --check
```

## Open questions
`GetEnumItems` and `typeof(Enum.Material) == "Enum"` are not implemented either.

## Last check results (merge `0a8f9d2` on main `d5d6657`)
`cargo test -p metallic_core -p metallic_script -j 4 --no-fail-fast`: 627 passed, 0 failed, 4 ignored.
`cargo test --workspace -j 4 --no-fail-fast`: 2450 passed, 0 failed, 10 ignored. `cargo clippy --workspace --all-targets -j 4 --
-D warnings`: clean. `cargo deny check licenses`: licenses ok. `cargo fmt --all -- --check` and the class-file rustfmt step: clean.
An earlier run overlapped another agent's artificial load (#497), failed on script timeouts and was discarded.
