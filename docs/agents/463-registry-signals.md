# #463 signals declared in the class registry (step 6 of the class-system migration)

Branch `studio/463-registry-signals`. Design: `docs/design/class-system.md` (sections 0, 3.2, 6, 13, 14 row #463, 15).

## Goal
Signals are class members: declared in class files with typed parameters and context, inherited, keyed by
`(InstanceId, SignalId, arg)`, fired generically, connected through `HostApi`. Scripts cannot tell, except that `Changed`,
`GetPropertyChangedSignal`, `Once` and `Wait` now exist.

## Done
- Core: `SignalDesc` builders, `Ty::Signal`, `ScriptValue::Signal`, `SignalId`, `property_signal_arg`, `SignalQueue` + `World::fire`,
  `MethodCall::fire`, `World::property_changes_since`, `ConnectionId::allocate`.
- Class files: every signal scripts have today, plus `Instance.Changed` and `Instance:GetPropertyChangedSignal`.
- Script: `EventKind`, `event_kind`, `GuiEventKind::of` removed. `HostApi::connect_signal`/`disconnect`, `WaitRequest::Signal`,
  `signal:Once`, `signal:Wait`, `api/property_signals.rs`.

## Next
See the end of this file once finished: tests, docs, perf, final checks.
