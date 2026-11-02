//! `obj:Method()` allocates nothing per call once warm (design section 4.2): the function comes from
//! the cache, the arguments go in a per-VM buffer.
//!
//! A batch's fixed cost (the call into the VM from Rust) is measured by a batch of nothing and taken
//! off, so a difference is what the calls themselves allocated. Strings and instance wrappers made
//! on first use are warmed first.

mod fixture_classes;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use fixture_classes::Fixture;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

fn count() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

// Counting is thread-local so unrelated test-runner activity cannot make this flaky.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: forward the allocator contract unchanged to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: forward the allocator contract unchanged to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout are supplied by the GlobalAlloc caller.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        // SAFETY: forward the allocator contract unchanged to System.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations(operation: impl FnOnce()) -> usize {
    ALLOCATIONS.set(0);
    COUNTING.set(true);
    operation();
    COUNTING.set(false);
    ALLOCATIONS.get()
}

const CALLS: u32 = 2_000;

/// What a batch of `body` costs beyond a batch of nothing, once warm. `body` works on an instance
/// of `class` named `object`.
fn per_batch(class: &str, body: &str) -> usize {
    let fixture = Fixture::server();
    let id = fixture
        .world
        .borrow_mut()
        .create(class, None)
        .expect("create");
    let object = fixture.api.wrap(id).unwrap();
    let batch = fixture.function(&format!(
        "return function(object, n)\n    for _ = 1, n do {body} end\nend"
    ));
    let run = |n: u32| {
        let result: Result<(), _> = fixture.vm.call_with_budget(&batch, (object.clone(), n));
        black_box(result).unwrap_or_else(|e| panic!("{body}: {}", e.message));
    };
    for _ in 0..8 {
        run(CALLS);
    }
    // The smallest of a few: a GC step or table growth outside the calls adds noise upwards only.
    let empty = (0..4).map(|_| allocations(|| run(0))).min().unwrap();
    let full = (0..4).map(|_| allocations(|| run(CALLS))).min().unwrap();
    full.saturating_sub(empty)
}

#[test]
fn a_declared_method_call_allocates_nothing() {
    let cost = |body| per_batch("Part", body);
    assert_eq!(cost("object:Nothing()"), 0, "no arguments, no results");
    assert_eq!(cost("object:AddOne(1)"), 0, "a number in, a number out");
    assert_eq!(cost("object:AddOne(1.5)"), 0, "a fraction in and out");
}

#[test]
fn a_bridged_method_call_allocates_nothing() {
    assert_eq!(per_batch("Model", "object:GetScale()"), 0);
}

#[test]
fn text_costs_one_allocation_where_it_is_read() {
    // mlua gives a Lua string read from Rust a shared counter the first time it is borrowed, which
    // is one allocation per read of an argument or a key. A method name in `obj:Method()` is read
    // from the VM directly and has no such cost; a text argument and a field access do.
    let at_most_one = CALLS as usize;
    assert!(per_batch("Part", "object:IsA('BasePart')") <= at_most_one);
    assert!(per_batch("Part", "object:FindFirstChild('Missing')") <= at_most_one);
    assert!(per_batch("Part", "local f = object.Nothing") <= at_most_one);
    assert!(per_batch("Part", "local f = object.IsA") <= at_most_one);
}

#[test]
fn the_count_sees_an_allocating_call() {
    // Guards the measurement: a string result does allocate.
    assert!(per_batch("Part", "object:Greet()") >= CALLS as usize);
}
