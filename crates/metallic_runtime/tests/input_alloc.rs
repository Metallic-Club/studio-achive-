#![cfg(feature = "presentation")]
//! `InputRuntime::evaluate` (and `push`, `drain_events`) must not allocate once the runtime is
//! built (input spec section 13). A counting global allocator checks it on the reference load:
//! 4 contexts, 32 actions, 96 bindings, 40 raw events a frame.
use metallic_runtime::InputRuntime;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[path = "common/reference_load.rs"]
mod reference_load;

struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    let _ = ON.try_with(|on| {
        if on.get() {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
    });
}

// SAFETY: forwards every call to the system allocator; it only counts calls on the thread that
// switched counting on.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn evaluate_makes_no_heap_allocation() {
    let mut rt = InputRuntime::new(&reference_load::config());
    let dt = 1.0 / 60.0;
    // Warm up so every path has run once.
    for f in 0..120u64 {
        let t = 10.0 + f as f64 * dt;
        reference_load::push_frame(&mut rt, f, t);
        rt.evaluate(t, dt as f32);
        rt.drain_events().for_each(drop);
    }
    COUNT.with(|c| c.set(0));
    ON.with(|on| on.set(true));
    let mut events = 0;
    for f in 120..720u64 {
        let t = 10.0 + f as f64 * dt;
        reference_load::push_frame(&mut rt, f, t);
        rt.evaluate(t, dt as f32);
        events += rt.drain_events().count();
    }
    let allocations = COUNT.with(Cell::get);
    // The counter works: a plain allocation is seen.
    drop(std::hint::black_box(vec![0u8; 64]));
    assert_eq!(
        COUNT.with(Cell::get),
        allocations + 1,
        "the counting allocator is live"
    );
    ON.with(|on| on.set(false));
    assert!(events > 1000, "the load did something: {events} events");
    assert_eq!(allocations, 0, "push, evaluate and drain_events allocated");
}

/// Nobody drains the events (the Player did not until #251 reads them): the buffer stays bounded,
/// nothing allocates, the oldest events are dropped and counted, and vector state keeps updating.
#[test]
fn undrained_events_stay_bounded_and_state_keeps_updating() {
    let mut rt = InputRuntime::new(&reference_load::config());
    let stick = rt.find_action("Ctx3/Action2").expect("a stick action");
    let dt = 1.0 / 60.0;
    for f in 0..120u64 {
        let t = 10.0 + f as f64 * dt;
        reference_load::push_frame(&mut rt, f, t);
        rt.evaluate(t, dt as f32);
    }
    COUNT.with(|c| c.set(0));
    ON.with(|on| on.set(true));
    let mut last = [0.0f32; 2];
    let mut changes = 0;
    for f in 120..2000u64 {
        let t = 10.0 + f as f64 * dt;
        reference_load::push_frame(&mut rt, f, t);
        rt.evaluate(t, dt as f32);
        let v = rt.axis2(stick);
        changes += usize::from(v != last);
        last = v;
    }
    ON.with(|on| on.set(false));
    assert_eq!(COUNT.with(Cell::get), 0, "undrained evaluate allocated");
    assert!(changes > 100, "the stick state kept changing: {changes}");
    assert!(rt.dropped_events() > 0);
    assert!(rt.drain_events().count() <= 1024, "bounded");
}

#[test]
fn input_frame_makes_no_heap_allocation_once_warm() {
    use metallic_core::{Value, World};
    use metallic_runtime::input::{Control, Device, RawEvent};
    use metallic_runtime::{Game, StdoutSink};
    use std::rc::Rc;

    // A scene action on Space with no script: events and values still go to the script host.
    let mut w = World::new_with_services();
    let rs = w.root("ReplicatedStorage").unwrap();
    let ctx = w.create("InputContext", Some(rs)).unwrap();
    let action = w.create("InputAction", Some(ctx)).unwrap();
    let key = w.create("InputBinding", Some(action)).unwrap();
    w.set(key, "KeyCode", Value::String("Space".into()))
        .unwrap();
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("alloc".into(), w, Rc::new(StdoutSink));
    game.start().unwrap();
    let mut rt = InputRuntime::from_world(&game.world().borrow());
    let dt = 1.0 / 60.0;
    let mut frame = |rt: &mut InputRuntime, f: u64| {
        let t = 10.0 + f as f64 * dt;
        rt.push(RawEvent::button(
            Device::KeyboardMouse,
            Control::Space,
            if f.is_multiple_of(2) { 1.0 } else { 0.0 },
            t,
        ));
        game.input_frame(rt, t, dt as f32);
    };
    for f in 0..200u64 {
        frame(&mut rt, f);
    }
    COUNT.with(|c| c.set(0));
    ON.with(|on| on.set(true));
    for f in 200..400u64 {
        frame(&mut rt, f);
    }
    ON.with(|on| on.set(false));
    assert_eq!(COUNT.with(Cell::get), 0, "input_frame allocated");
}

/// The touch path allocates nothing per frame either: fingers on the stick, the look pad and a
/// button go through `TouchControls` into the runtime (spec 13).
#[test]
fn touch_controls_make_no_heap_allocation() {
    use metallic_runtime::input::touch::{DeviceProfile, TouchControls, TouchPhase};
    let mut rt = InputRuntime::default();
    let mut tc = TouchControls::new(DeviceProfile::phone());
    tc.sync(&mut rt, 0.0);
    let jump = tc.layout().buttons[0].center;
    let stick = tc.layout().stick.expect("stick").center;
    let dt = 1.0 / 60.0;
    let frame = |rt: &mut InputRuntime, tc: &mut TouchControls, f: u64| {
        let t = 10.0 + f as f64 * dt;
        tc.sync(rt, t);
        let phase = |down: bool| {
            if down {
                TouchPhase::Down
            } else {
                TouchPhase::Up
            }
        };
        let k = (f % 60) as f32;
        if f.is_multiple_of(60) {
            tc.touch(rt, 0, TouchPhase::Down, stick, t);
        }
        tc.touch(rt, 0, TouchPhase::Move, [stick[0] + k, stick[1] - k], t);
        if f % 60 == 59 {
            tc.touch(rt, 0, TouchPhase::Up, stick, t);
        }
        tc.touch(rt, 1, phase(f.is_multiple_of(2)), jump, t);
        tc.touch(rt, 2, TouchPhase::Down, [600.0, 100.0], t);
        tc.touch(rt, 2, TouchPhase::Move, [600.0 + k, 100.0], t);
        rt.evaluate(t, dt as f32);
        rt.drain_events().count()
    };
    for f in 0..120u64 {
        frame(&mut rt, &mut tc, f);
    }
    COUNT.with(|c| c.set(0));
    ON.with(|on| on.set(true));
    let mut events = 0;
    for f in 120..720u64 {
        events += frame(&mut rt, &mut tc, f);
    }
    ON.with(|on| on.set(false));
    assert!(events > 600, "the fingers did something: {events} events");
    assert_eq!(COUNT.with(Cell::get), 0, "the touch path allocated");
}

/// With player overrides applied (a toggle, stick settings), frames and cached glyph lookups
/// allocate nothing once warm (#254).
#[test]
fn overrides_toggles_and_glyph_lookups_make_no_heap_allocation() {
    use metallic_runtime::input::overrides::{Stick, StickSettings};
    use metallic_runtime::input::{
        Control, Device, DeviceKind, InputConfig, MENU_ACTION, PlayerOverrides, RawEvent,
    };
    let mut o = PlayerOverrides::default();
    o.action_mut("CharacterControls/Run").toggle = Some(true);
    o.set_stick(
        Stick::Right,
        StickSettings {
            sensitivity: 1.5,
            invert_y: true,
            ..Default::default()
        },
    );
    let mut rt = InputRuntime::new(&InputConfig::player_defaults());
    rt.set_overrides(o);
    let ids: Vec<_> = [
        "CharacterControls/Jump",
        "CharacterControls/Run",
        MENU_ACTION,
    ]
    .iter()
    .map(|p| rt.find_action(p).unwrap())
    .collect();
    let dt = 1.0 / 60.0;
    let frame = |rt: &mut InputRuntime, f: u64| {
        let t = 10.0 + f as f64 * dt;
        let v = if f.is_multiple_of(7) { 1.0 } else { 0.0 };
        rt.push(RawEvent::button(
            Device::KeyboardMouse,
            Control::LeftShift,
            v,
            t,
        ));
        let pad = Device::Gamepad(0);
        rt.push(RawEvent::vector(pad, Control::Thumbstick2, [0.5, 0.2], t));
        rt.frame(t, dt as f32);
        rt.drain_events().for_each(drop);
        let mut n = 0;
        for &id in &ids {
            n += rt.glyph(id, None).map_or(0, |g| usize::from(g.count));
            n += rt.glyphs(id, DeviceKind::Gamepad).len();
        }
        std::hint::black_box(n);
    };
    for f in 0..120u64 {
        frame(&mut rt, f);
    }
    COUNT.with(|c| c.set(0));
    ON.with(|on| on.set(true));
    for f in 120..600u64 {
        frame(&mut rt, f);
    }
    ON.with(|on| on.set(false));
    assert_eq!(
        COUNT.with(Cell::get),
        0,
        "frames or glyph lookups allocated"
    );
}
