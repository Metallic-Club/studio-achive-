//! Round-2 hardening tests: bounded pattern matching, weak budget handle, error reporting.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use metallic_script::{Context, OutputSink, ScriptVm};
use mlua::Function;

#[derive(Default)]
struct Capture {
    lines: RefCell<Vec<String>>,
}

impl OutputSink for Capture {
    fn print(&self, line: &str) {
        self.lines.borrow_mut().push(format!("print:{line}"));
    }
    fn warn(&self, line: &str) {
        self.lines.borrow_mut().push(format!("warn:{line}"));
    }
    fn error(&self, line: &str) {
        self.lines.borrow_mut().push(format!("error:{line}"));
    }
}

fn vm() -> (ScriptVm, Rc<Capture>) {
    metallic_script::use_test_budgets_on_this_thread();
    let sink = Rc::new(Capture::default());
    (
        ScriptVm::with_output(Context::Server, sink.clone()).unwrap(),
        sink,
    )
}

/// A VM on the real limits, for the tests of those limits.
fn strict_vm() -> (ScriptVm, Rc<Capture>) {
    let (vm, sink) = vm();
    vm.set_resume_budget(metallic_script::RESUME_BUDGET);
    vm.set_step_budget_sec(metallic_script::STEP_BUDGET_SEC);
    (vm, sink)
}

#[test]
fn single_quantifier_pathological_find_times_out_or_errors_fast() {
    let (mut vm, _) = strict_vm();
    let start = Instant::now();
    for src in [
        "string.find(string.rep('a', 1e6), '.-b')",
        "string.match(string.rep('a', 1e6), '.*b')",
        "string.gsub(string.rep('a', 1e6), '.-b', 'x')",
        "for _ in string.gmatch(string.rep('a', 1e6), '.-b') do end",
        "string.find(string.rep('a', 100000), 'a-a-a-a-a-b')",
    ] {
        let err = vm.run_script("T", src).unwrap_err();
        assert!(err.message.starts_with("T:1:"), "{src}: {}", err.message);
        assert!(
            err.message.contains("pattern too complex") || err.message.contains("Script timeout"),
            "{src}: {}",
            err.message
        );
    }
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    vm.run_script("T", "print('alive')").unwrap();
}

#[test]
fn trim_pattern_on_100kb_works() {
    let (mut vm, _) = vm();
    let start = Instant::now();
    vm.run_script(
        "T",
        "local s = '  ' .. string.rep('x', 100000) .. '  '
         assert(#string.match(s, '^%s*(.-)%s*$') == 100000)
         assert(#string.gsub(s, '^%s+', '') == 100002)
         assert(string.find(s, 'x+') == 3)",
    )
    .unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn nan_init_is_treated_as_start() {
    let (mut vm, _) = vm();
    vm.run_script(
        "T",
        "assert(string.find('abc', 'b', 0/0) == 2)
         assert(string.match('abc', '.', 0/0) == 'a')",
    )
    .unwrap();
}

const FMT: &str = "local function fmt(...) local o = {} for i = 1, select('#', ...) do o[#o + 1] = tostring((select(i, ...))) end return table.concat(o, '|') end
local function it(f, s, p) local r = {} for a, b in f(s, p) do r[#r + 1] = tostring(a) .. ',' .. tostring(b) end return table.concat(r, ';') end
";

#[test]
fn pattern_results_match_luau_for_common_cases() {
    let cases = [
        "fmt(string.find('hello world', 'wor'))",
        "fmt(string.find('hello', 'l+'))",
        "fmt(string.find('hello', '(l)(l)'))",
        "fmt(string.find('hello', '()ll()'))",
        "fmt(string.find('hello', 'xyz'))",
        "fmt(string.find('hello', '^h'))",
        "fmt(string.find('hello', '^e'))",
        "fmt(string.find('hello', 'o$'))",
        "fmt(string.find('a.b', '.', 1, true))",
        "fmt(string.find('abc', 'b', -1))",
        "fmt(string.find('abc', '', 10))",
        "fmt(string.find('abc', '', 4))",
        "fmt(string.match('key=value', '(%w+)=(%w+)'))",
        "fmt(string.match('  trim  ', '^%s*(.-)%s*$'))",
        "fmt(string.match('THE (quick) fox', '%((%a+)%)'))",
        "fmt(string.match('x(a(b)c)y', '%b()'))",
        "fmt(string.match('THE (quick) fox', '%f[%a]%a+', 5))",
        "fmt(string.match('hello', '.-'))",
        "fmt(string.match('2024-10-30', '(%d+)-(%d+)-(%d+)'))",
        "fmt(string.match('abc', 'a?b?c?d?'))",
        "fmt(string.match('aaa', '(a)%1'))",
        "fmt(string.match('abab', '(ab)%1'))",
        "fmt(string.match('[test]', '%[(.-)%]'))",
        "fmt(string.match('foo.bar', '[^.]+'))",
        "fmt(string.match('x', '[a-c]'))",
        "fmt(string.match('a-b', '[%a-]+'))",
        "fmt(string.match('hello123', '%a+()'))",
        "fmt(string.gsub('hello world', 'o', '0'))",
        "fmt(string.gsub('hello', 'l', 'L', 1))",
        "fmt(string.gsub('abc', '%w', '%0%0'))",
        "fmt(string.gsub('hello world', '(%w+) (%w+)', '%2 %1'))",
        "fmt(string.gsub('abc', '', '-'))",
        "fmt(string.gsub('hello', '.', {h = 'H', e = false}))",
        "fmt(string.gsub('abc', '%w', function(c) return c:upper() .. '!' end))",
        "fmt(string.gsub('abc', 'b', '%%'))",
        "fmt(string.gsub('  x  ', '^%s+', ''))",
        "fmt(string.gsub('a,b,,c', ',', ';'))",
        "fmt(string.gsub('abc', '(b)', '[%1]'))",
        "fmt(string.gsub('abc', 'b', 5))",
        "fmt(string.gsub('abc', '[', ''))",
        "fmt(string.find('abc', '%'))",
        "it(string.gmatch, 'one two  three', '%a+')",
        "it(string.gmatch, 'k1=v1,k2=v2', '(%w+)=(%w+)')",
        "it(string.gmatch, 'abc', '%a*')",
        "it(string.gmatch, 'abc', '()')",
        "it(string.gmatch, 'a b', '%f[%w]%w+')",
        "it(string.gmatch, 'aXb', 'x')",
    ];
    let reference = mlua::Lua::new();
    let (mut vm, sink) = vm();
    for case in cases {
        let call = format!("local ok, r = pcall(function() return {case} end) ");
        let expected: String = reference
            .load(format!("{FMT}{call}return ok and r or 'ERR'"))
            .eval()
            .unwrap();
        sink.lines.borrow_mut().clear();
        vm.run_script("T", &format!("{FMT}{call}print(ok and r or 'ERR')"))
            .unwrap();
        assert_eq!(
            sink.lines.borrow()[0],
            format!("print:{expected}"),
            "case: {case}"
        );
    }
}

#[test]
fn string_rep_with_string_count_is_bounded() {
    let (mut vm, _) = vm();
    let start = Instant::now();
    let err = vm
        .run_script("T", "string.rep('x', '100000000')")
        .unwrap_err();
    assert!(
        err.message.starts_with("T:1:") && err.message.contains("too large"),
        "{}",
        err.message
    );
    assert!(
        vm.run_script("T", "string.rep(12345, '100000000')")
            .is_err()
    );
    assert!(vm.run_script("T", "('x'):rep(1e300)").is_err());
    vm.run_script(
        "T",
        "assert(('x'):rep('3') == 'xxx' and string.rep('ab', 2) == 'abab')",
    )
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
}

struct DropSink(Rc<RefCell<u32>>);

impl OutputSink for DropSink {
    fn print(&self, _: &str) {}
    fn warn(&self, _: &str) {}
    fn error(&self, _: &str) {}
}

impl Drop for DropSink {
    fn drop(&mut self) {
        *self.0.borrow_mut() += 1;
    }
}

#[test]
fn vm_is_freed_after_shutdown_even_with_budget_closures() {
    let drops = Rc::new(RefCell::new(0));
    let vm = ScriptVm::with_output(Context::Server, Rc::new(DropSink(drops.clone()))).unwrap();
    let budget = vm.budget();
    let host = vm
        .lua()
        .create_function(move |_, f: Function| {
            budget.call::<()>(&f, ()).map_err(mlua::Error::external)
        })
        .unwrap();
    vm.set_global("host_call", host).unwrap();
    vm.shutdown();
    assert_eq!(
        *drops.borrow(),
        1,
        "VM (and its sink) leaked through the Budget closure"
    );
}

#[test]
fn budget_errors_cleanly_when_vm_is_gone() {
    let (vm, _) = vm();
    let budget = vm.budget();
    let f = vm.lua().create_function(|_, ()| Ok(())).unwrap();
    let thread = vm.lua().create_thread(f).unwrap();
    drop(vm);
    let err = budget.resume(&thread, ()).unwrap_err();
    assert!(err.message.contains("no longer exists"), "{}", err.message);
}

#[test]
fn timeout_reported_once() {
    let (mut vm, sink) = strict_vm();
    // A spawned thread times out inside run_script: one error, returned, none in the sink.
    let err = vm
        .run_script("T", "task.spawn(function() while true do end end)")
        .unwrap_err();
    assert!(err.message.contains("Script timeout"), "{}", err.message);
    assert!(sink.lines.borrow().is_empty(), "{:?}", sink.lines.borrow());
    // In a stepped thread: exactly one sink line.
    vm.run_script(
        "U",
        "task.delay(1, function() task.spawn(function() while true do end end) end)",
    )
    .unwrap();
    vm.step(2.0);
    assert_eq!(sink.lines.borrow().len(), 1, "{:?}", sink.lines.borrow());
    sink.lines.borrow_mut().clear();
    // A pcall-caught timeout followed by a yield is reported once and the thread is dropped.
    vm.run_script(
        "V",
        "task.delay(1, function() pcall(function() while true do end end) task.wait(1) print('after') end)",
    )
    .unwrap();
    vm.step(3.0);
    vm.step(10.0);
    assert_eq!(sink.lines.borrow().len(), 1, "{:?}", sink.lines.borrow());
    assert!(sink.lines.borrow()[0].starts_with("error:V:1: Script timeout"));
}

/// Runs `src` and asserts it stops with a pattern/timeout error within 0.5 s (#166).
fn assert_pattern_stops_fast(src: &str) {
    let (mut vm, _) = strict_vm();
    let start = Instant::now();
    let err = vm.run_script("T", src).unwrap_err();
    let took = start.elapsed();
    assert!(
        err.message.contains("pattern too complex") || err.message.contains("Script timeout"),
        "{}",
        err.message
    );
    assert!(took < Duration::from_secs(5), "took {took:?}");
    vm.run_script("T", "assert(string.match('abc', '[a-c]+') == 'abc')")
        .unwrap();
}

#[test]
fn long_bracket_class_star_is_bounded() {
    // A 1 MB class whose only match is its last byte, repeated over 10k characters.
    assert_pattern_stops_fast(
        "local class = string.rep('b', 1000000) .. 'a'
         string.find(string.rep('a', 10000), '[' .. class .. ']*$')",
    );
}

#[test]
fn long_bracket_class_lazy_is_bounded() {
    assert_pattern_stops_fast(
        "local class = string.rep('b', 1000000) .. 'a'
         string.find(string.rep('a', 100000), '[' .. class .. ']-c')",
    );
}

/// Runs `src` on data built by `build` in a plain, unbudgeted Luau state, like the old
/// `table.sort`, and returns how long it took.
fn plain_luau_time(build: &str, src: &str) -> Duration {
    let lua = mlua::Lua::new();
    let big: mlua::Table = lua.load(build).eval().unwrap();
    lua.globals().set("big", big).unwrap();
    let start = Instant::now();
    // The plain state may end in a memory error; only the time it ran matters.
    let _ = lua.load(src).exec();
    start.elapsed()
}

/// Builds the data once for the sandbox and once for a plain Luau state; the plain state shows
/// how long the C sort needs (at least `min_plain`, so the workload means something), and the
/// budgeted run must stop with a timeout in less than half of that (#274). On the old code the
/// sort could not be interrupted and took the full time.
fn assert_stops_early(build: &str, src: &str, min_plain: Duration) {
    let plain = plain_luau_time(build, src);
    assert!(
        plain >= min_plain,
        "workload too small: plain sort took {plain:?}"
    );
    let (mut vm, _) = vm();
    let big: mlua::Table = vm.lua().load(build).eval().unwrap();
    vm.set_global("big", big).unwrap();
    vm.set_resume_budget(Duration::from_millis(10));
    let start = Instant::now();
    let err = vm.run_script("T", src).unwrap_err();
    let took = start.elapsed();
    assert!(err.message.contains("Script timeout"), "{}", err.message);
    assert!(
        took < plain / 2,
        "stopped after {took:?}, a plain C sort takes {plain:?}"
    );
    vm.set_resume_budget(Duration::from_secs(2));
    vm.run_script("T", "local t = {3, 1, 2} table.sort(t) assert(t[1] == 1)")
        .unwrap();
}

#[test]
fn big_string_sort_with_shared_prefix_is_interruptible() {
    // Every comparison is a long memcmp in C.
    assert_stops_early(
        "local p = string.rep('a', 1500)
         local big = {}
         for i = 1, 150000 do big[i] = p .. ((i * 7919) % 150001) end
         return big",
        "table.sort(big)",
        Duration::from_millis(150),
    );
}

#[test]
fn big_sort_with_a_c_comparator_is_interruptible() {
    // rawequal never returns to Lua: with only C calls the interrupt could not fire.
    assert_stops_early(
        "local big = table.create(8000000, 0) for i = 1, 8000000 do big[i] = i end return big",
        "table.sort(big, rawequal)",
        Duration::from_secs(1),
    );
}

#[test]
fn big_sort_with_a_c_lt_metamethod_is_interruptible() {
    assert_stops_early(
        "local mt = {__lt = rawequal}
         local big = {}
         for i = 1, 1000000 do big[i] = setmetatable({}, mt) end
         return big",
        "table.sort(big)",
        Duration::from_millis(150),
    );
}

#[test]
fn huge_number_sort_without_comparator_is_interruptible() {
    // Above the C-sort cap numbers also go through the Lua closure.
    let (mut vm, _) = vm();
    let big: mlua::Table = vm
        .lua()
        .load("local big = table.create(2000000, 0) for i = 1, 2000000 do big[i] = (i * 7919) % 2000003 end return big")
        .eval()
        .unwrap();
    vm.set_global("big", big).unwrap();
    vm.set_resume_budget(Duration::from_millis(40));
    let start = Instant::now();
    let err = vm.run_script("T", "table.sort(big)").unwrap_err();
    assert!(err.message.contains("Script timeout"), "{}", err.message);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn big_sort_with_a_lua_comparator_stays_interruptible() {
    let (mut vm, _) = vm();
    let big: mlua::Table = vm
        .lua()
        .load("return table.create(300000, 1)")
        .eval()
        .unwrap();
    vm.set_global("big", big).unwrap();
    vm.set_resume_budget(Duration::from_millis(40));
    let start = Instant::now();
    let err = vm
        .run_script("T", "table.sort(big, function(a, b) while true do end end)")
        .unwrap_err();
    assert!(err.message.contains("Script timeout"), "{}", err.message);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn sorting_200k_numbers_without_a_comparator_fits_the_default_budget() {
    let (mut vm, _) = vm();
    let big: mlua::Table = vm
        .lua()
        .load("local t = table.create(200000, 0) for i = 1, 200000 do t[i] = (i * 7919) % 200003 end return t")
        .eval()
        .unwrap();
    vm.set_global("big", big).unwrap();
    // The sort alone must fit a 1 s budget (the default is 0.1 s; loaded CI machines need slack):
    // it runs in C for numbers, not through 3.4M Lua calls.
    vm.set_resume_budget(Duration::from_secs(1));
    let start = Instant::now();
    vm.run_script("T", "table.sort(big)").unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    vm.set_resume_budget(Duration::from_secs(5));
    vm.run_script("T", "for i = 2, 200000 do assert(big[i - 1] <= big[i]) end")
        .unwrap();
}

#[test]
fn normal_sorts_behave_as_before() {
    let (mut vm, _) = vm();
    vm.set_resume_budget(Duration::from_secs(20));
    vm.run_script(
        "T",
        "local t = {5, 2, 9, 1, 7}
         table.sort(t)
         assert(table.concat(t, ',') == '1,2,5,7,9')
         table.sort(t, function(a, b) return a > b end)
         assert(table.concat(t, ',') == '9,7,5,2,1')
         local w = {'b', 'a', 'c'}
         table.sort(w)
         assert(w[1] == 'a' and w[3] == 'c')
         -- tables above the Lua-closure threshold sort the same way, with or without a comparator
         local big, n = {}, 5000
         for i = 1, n do big[i] = (i * 7919) % 10007 end
         local copy = table.clone(big)
         table.sort(big)
         for i = 2, n do assert(big[i - 1] <= big[i]) end
         table.sort(copy, function(a, b) return a > b end)
         for i = 2, n do assert(copy[i - 1] >= copy[i]) end
         local up = {}
         for i = 1, n do up[i] = 'k' .. (i * 31 % 977) end
         table.sort(up)
         for i = 2, n do assert(up[i - 1] <= up[i]) end
         -- Luau's own errors
         local ok = pcall(table.sort, 5)
         assert(not ok)
         local mixed = {}
         for i = 1, n do mixed[i] = i end
         mixed[n // 2] = 'x'
         local ok2, e2 = pcall(table.sort, mixed)
         assert(not ok2 and e2:find('attempt to compare') and not e2:find('engine'), e2)
         local bad = {}
         for i = 1, n do bad[i] = i % 7 end
         local ok3, e3 = pcall(table.sort, bad, function(a, b) return true end)
         assert(not ok3 and e3:find('invalid order function'), e3)
         -- a comparator error keeps its own position
         local ok4, e4 = pcall(table.sort, bad, function(a, b) error('boom') end)
         assert(not ok4 and e4:find('boom'))",
    )
    .unwrap();
}

#[test]
fn gsub_replacement_loop_is_bounded() {
    // 1M empty matches x 64 bytes: ~64 MB appended, about 10 s unbounded in a debug build.
    let (mut vm, _) = strict_vm();
    let start = Instant::now();
    let err = vm
        .run_script(
            "T",
            "local s = string.rep('a', 1000000)
             string.gsub(s, '', string.rep('x', 64))",
        )
        .unwrap_err();
    assert!(
        err.message.contains("Script timeout") || err.message.contains("pattern too complex"),
        "{}",
        err.message
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    vm.run_script("T", "assert(string.gsub('abc', '', '-') == '-a-b-c-')")
        .unwrap();
}

#[test]
fn back_reference_compares_are_charged() {
    assert_pattern_stops_fast(
        "local s = string.rep('a', 60000)
         string.find(s, '(a*)%1b')",
    );
}

#[test]
fn gsub_charges_replacement_bytes_scanned_not_only_appended() {
    // '%0' on matches that are all empty appends nothing, yet the 64 MB replacement is walked
    // for every match: 20 matches x 64M bytes.
    let (mut vm, _) = strict_vm();
    let start = Instant::now();
    let err = vm
        .run_script(
            "T",
            "local r = string.rep('%0', 32000000)
             string.gsub(string.rep('a', 20), '', r)",
        )
        .unwrap_err();
    assert!(
        err.message.contains("Script timeout") || err.message.contains("pattern too complex"),
        "{}",
        err.message
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

// ---- #344: the remaining unbounded C calls ----

#[test]
fn concat_of_millions_of_elements_is_interruptible() {
    assert_stops_early(
        "local t = table.create(5000000, 0) for i = 1, 5000000 do t[i] = i end return t",
        "table.concat(big)",
        Duration::from_millis(400),
    );
}

/// `table.concat(t, ...)` for each argument list, as (ok, text) with the leading `chunk:line:`
/// position of an error removed, run by the sandbox and by a plain Luau state (the raw C
/// function).
fn concat_outcomes(lua_calls: &[String], run: impl Fn(&str) -> String) -> Vec<String> {
    lua_calls.iter().map(|c| run(c)).collect()
}

#[test]
fn concat_behaves_exactly_like_the_raw_c_function() {
    const SETUP: &str = "
        local function strip(e) return (tostring(e):gsub('^[^:]*:%d+: ', '')) end
        function show(...)
            local ok, r = pcall(table.concat, ...)
            return tostring(ok) .. '|' .. (ok and #r .. ':' .. r:sub(1, 20) .. r:sub(-20) or strip(r))
        end
        function raw(...)
            local ok, r = pcall(table.concat, ...)
            return tostring(ok) .. '|' .. tostring(#tostring(r) < 100 and r or #r)
        end
        function show_called(...)
            local args = table.pack(...)
            local ok, r = pcall(function() return table.concat(table.unpack(args, 1, args.n)) end)
            return tostring(ok) .. '|' .. (ok and #r or strip(r))
        end
        function mkz(n) local t = mk(n) t[0] = 'z' t[-1] = 'y' return t end
        function mk(n, holes)
            local t = {}
            for i = 1, n do t[i] = (i % 7) .. 'x' end
            if holes then t[holes] = nil end
            return t
        end";
    let sizes = ["0", "1", "8193", "50000"];
    let bounds = [
        "nil",
        "0",
        "2",
        "1.5",
        "-3",
        "8193",
        "20000",
        "49999",
        "50001",
        "2^31",
        "2^53",
        "0/0",
        "math.huge",
        "-math.huge",
        "'2'",
        "true",
        "49999.5",
        "50000.9",
        "'49999'",
        "' 49999 '",
        "-0.5",
        "1e4",
        "2147483647.5",
        "-2147483648.5",
        "2^31 - 1",
    ];
    let mut calls = Vec::new();
    for n in sizes {
        for hole in ["nil", "9000", "49999"] {
            for sep in ["nil", "','"] {
                for i in ["nil", "1.5", "-3", "0/0", "8193"] {
                    for j in bounds {
                        calls.push(format!("show(mk({n}, {hole}), {sep}, {i}, {j})"));
                    }
                }
            }
            for sep in ["5", "''", "{}"] {
                calls.push(format!("show(mk({n}, {hole}), {sep})"));
            }
        }
    }
    // A value at index 0 (and -1): a start or end the platform reads as 0 or -1 walks it.
    for n in ["1", "8193", "50000"] {
        for i in [
            "0/0",
            "math.huge",
            "-math.huge",
            "2^53",
            "-2^53",
            "2^31",
            "'1e30'",
            "'-1e30'",
            "nil",
        ] {
            for j in [
                "nil",
                "0/0",
                "math.huge",
                "-math.huge",
                "2^53",
                "-2^53",
                "'1e30'",
                "-1",
                "0",
            ] {
                calls.push(format!("show(mkz({n}), ',', {i}, {j})"));
            }
        }
    }
    // Errors raised under pcall(table.concat, ...) carry no position at all, exactly as before.
    for n in ["3", "50000"] {
        for hole in ["nil", "2", "30000"] {
            for sep in ["nil", "','", "{}"] {
                calls.push(format!("raw(mk({n}, {hole}), {sep})"));
                calls.push(format!("raw(mk({n}, {hole}), {sep}, 1, 2)"));
            }
        }
    }
    calls.push("show_called(mk(50000, 30000), ',')".into());
    calls.push("show_called(mk(50000), ',', 3, 49000)".into());
    calls.push("show_called(mk(50000, 20000), ',', 3, 49000)".into());

    // Raw C function: a plain Luau state.
    let plain = mlua::Lua::new();
    plain.load(SETUP).set_name("=setup").exec().unwrap();
    let plain_run = |c: &str| plain.load(format!("return {c}")).eval::<String>().unwrap();
    let want = concat_outcomes(&calls, plain_run);

    let (mut vm, _) = vm();
    vm.set_resume_budget(Duration::from_secs(120));
    vm.lua().load(SETUP).set_name("=setup").exec().unwrap();
    // `show` and `mk` are plain globals of the engine's Lua state; the sandboxed library is
    // what `table.concat` resolves to there too.
    let vm_run = |c: &str| {
        vm.lua()
            .load(format!("return {c}"))
            .eval::<String>()
            .unwrap()
    };
    let got = concat_outcomes(&calls, vm_run);
    for (c, (w, g)) in calls.iter().zip(want.iter().zip(got.iter())) {
        assert_eq!(w, g, "{c}");
    }
    // Called from a script, the error is positioned at the script's own line.
    vm.set_resume_budget(Duration::from_secs(5));
    let err = vm
        .run_script(
            "T",
            "local t = {} for i = 1, 20000 do t[i] = 'x' end t[15000] = {}
             table.concat(t, ',')",
        )
        .unwrap_err();
    assert!(
        err.message
            .starts_with("T:2: invalid value (table) at index 15000"),
        "{}",
        err.message
    );
}

#[test]
fn concat_with_fractional_or_string_bounds_is_interruptible() {
    // A fractional or numeric-string bound is read by the C function as an integer, so the
    // range is as long as with an integer bound and must be chunked all the same.
    for bound in ["#big + 0.5", "tostring(#big)", "#big + 0.9", "'5e6'"] {
        assert_stops_early(
            "local t = table.create(5000000, 0) for i = 1, 5000000 do t[i] = i end return t",
            &format!("table.concat(big, ',', 1, {bound})"),
            Duration::from_millis(400),
        );
    }
}

#[test]
fn concat_chunk_boundaries_join_correctly() {
    let (mut vm, _) = vm();
    vm.set_resume_budget(Duration::from_secs(20));
    vm.run_script(
        "T",
        "for _, n in {0, 1, 2, 8191, 8192, 8193, 16384, 16385, 3 * 8192 + 7} do
             local t = {}
             for i = 1, n do t[i] = i % 10 .. '' end
             local acc = {}
             for i = 1, n do acc[#acc + 1] = t[i] end
             local expected = ''
             for i = 1, n do expected = expected .. t[i] .. (i < n and ',' or '') end
             assert(table.concat(t, ',') == expected, n)
             assert(table.concat({}, ',', 5, 1) == '')
         end",
    )
    .unwrap();
}

#[test]
fn huge_pack_formats_are_refused() {
    let (mut vm, _) = vm();
    vm.set_resume_budget(Duration::from_secs(5));
    let start = Instant::now();
    for call in ["pack(f)", "packsize(f)", "unpack(f, 'abc')"] {
        let err = vm
            .run_script(
                "T",
                &format!("local f = string.rep('x', 64 * 1024 * 1024) string.{call}"),
            )
            .unwrap_err();
        assert!(
            err.message.contains("format string too long"),
            "{call}: {}",
            err.message
        );
    }
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    // A short format with a huge item size: the padding would run until memory ran out.
    // Each item is small, the total is not: 300 x c999999 padded until memory ran out.
    for fmt in [
        "c200000000".to_string(),
        "<i4 c99999999".to_string(),
        "c999999".repeat(300),
        "c9999999".repeat(2),
        "c99999999999999999999".to_string(),
    ] {
        let err = vm
            .run_script("T", &format!("string.pack('{fmt}', 1, '')"))
            .unwrap_err();
        assert!(
            err.message.contains("result too large"),
            "{}: {}",
            &fmt[..fmt.len().min(30)],
            err.message
        );
    }
    vm.run_script(
        "T",
        "assert(#string.pack('c999999', 'x') == 999999)
         assert(#string.pack(string.rep('c999999', 10), 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j') == 9999990)",
    )
    .unwrap();
    vm.run_script(
        "T",
        "local p = string.pack('<i4 z s2', 7, 'hi', 'abc')
         local a, b, c = string.unpack('<i4 z s2', p)
         assert(a == 7 and b == 'hi' and c == 'abc')
         assert(string.packsize('<i4 i8') == 12)
         local big = string.rep('i1', 30000)
         assert(string.packsize(big) == 30000)",
    )
    .unwrap();
}

#[test]
fn huge_strings_are_refused_by_whole_string_passes() {
    let (mut vm, _) = vm();
    vm.set_resume_budget(Duration::from_secs(5));
    let start = Instant::now();
    vm.run_script(
        "T",
        "local s = string.rep('a', 16777217)
         for _, name in {'upper', 'lower', 'reverse'} do
             local ok, e = pcall(string[name], s)
             assert(not ok and e:find('string too long'), name)
             local ok2, e2 = pcall(function() return s[name](s) end)
             assert(not ok2)
         end
         local ok, e = pcall(utf8.len, s)
         assert(not ok and e:find('string too long'))
         local ok3, e3 = pcall(utf8.offset, s, 5)
         assert(not ok3 and e3:find('string too long'))",
    )
    .unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    vm.run_script(
        "T",
        "local s = string.rep('aB', 1000)
         assert(s:upper() == string.rep('AB', 1000) and s:lower() == string.rep('ab', 1000))
         assert(s:reverse() == string.rep('Ba', 1000))
         assert(utf8.len('h\u{e9}llo') == 5 and utf8.len('abc', 2) == 2)
         assert(utf8.len('\\xff') == nil)
         assert(string.upper(5) == '5')",
    )
    .unwrap();
}

#[test]
fn huge_split_subjects_are_refused() {
    let (mut vm, _) = vm();
    vm.set_resume_budget(Duration::from_secs(5));
    let start = Instant::now();
    let err = vm
        .run_script(
            "T",
            "local s = string.rep('a,', 2200000) string.split(s, ',')",
        )
        .unwrap_err();
    assert!(err.message.contains("string too long"), "{}", err.message);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    vm.run_script(
        "T",
        "local parts = string.split('a,b,,c', ',')
         assert(#parts == 4 and parts[1] == 'a' and parts[3] == '' and parts[4] == 'c')
         assert(#string.split(string.rep('x,', 400000), ',') == 400001)",
    )
    .unwrap();
}
