//! Linear pattern work must not consume one global backtracking allowance.

mod common;

use metallic_script::{Context, ScriptVm};
use mlua::{Function, LuaString, Table};
use std::time::{Duration, Instant};

const LARGE_INPUT: usize = 12_000_000;

fn string_function(vm: &ScriptVm, name: &str) -> Function {
    let strings: Table = vm.lua().globals().get("string").unwrap();
    strings.get(name).unwrap()
}

fn large_subject(vm: &ScriptVm) -> LuaString {
    vm.lua().create_string(vec![b'a'; LARGE_INPUT]).unwrap()
}

fn match_bytes(
    lua: &mlua::Lua,
    src: &[u8],
    pattern: &[u8],
    init: Option<f64>,
) -> Result<Option<Vec<u8>>, ()> {
    let strings: Table = lua.globals().get("string").unwrap();
    let function: Function = strings.get("match").unwrap();
    function
        .call::<Option<LuaString>>((
            lua.create_string(src).unwrap(),
            lua.create_string(pattern).unwrap(),
            init,
        ))
        .map(|value| value.map(|value| value.as_bytes().to_vec()))
        .map_err(|_| ())
}

fn replace_bytes(
    lua: &mlua::Lua,
    src: &[u8],
    pattern: &[u8],
    replacement: &[u8],
    max_n: Option<f64>,
) -> Result<(Vec<u8>, usize), ()> {
    let strings: Table = lua.globals().get("string").unwrap();
    let function: Function = strings.get("gsub").unwrap();
    function
        .call::<(LuaString, usize)>((
            lua.create_string(src).unwrap(),
            lua.create_string(pattern).unwrap(),
            lua.create_string(replacement).unwrap(),
            max_n,
        ))
        .map(|(value, count)| (value.as_bytes().to_vec(), count))
        .map_err(|_| ())
}

#[test]
fn single_byte_patterns_preserve_native_binary_and_magic_behavior() {
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let reference = mlua::Lua::new();
    let source: Vec<u8> = (0..=u8::MAX).collect();
    for byte in 0..=u8::MAX {
        assert_eq!(
            match_bytes(vm.lua(), &source, &[byte], None),
            match_bytes(&reference, &source, &[byte], None),
            "match pattern byte {byte}"
        );
        assert_eq!(
            replace_bytes(vm.lua(), &source, &[byte], b"Z", None),
            replace_bytes(&reference, &source, &[byte], b"Z", None),
            "gsub pattern byte {byte}"
        );
    }
}

#[test]
fn literal_match_start_bounds_keep_native_results() {
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let reference = mlua::Lua::new();
    let source = b"\xffa\0a\x80a";
    for init in [
        None,
        Some(-20.0),
        Some(-6.0),
        Some(-1.0),
        Some(0.0),
        Some(1.0),
        Some(2.9),
        Some(6.0),
        Some(7.0),
        Some(8.0),
    ] {
        for pattern in [b'a', 0, 0xff] {
            assert_eq!(
                match_bytes(vm.lua(), source, &[pattern], init),
                match_bytes(&reference, source, &[pattern], init),
                "init {init:?}, pattern {pattern}"
            );
        }
    }
}

#[test]
fn literal_gsub_maximum_count_and_replacement_bytes_keep_native_results() {
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let reference = mlua::Lua::new();
    let source = b"\xffa\0a\x80a";
    for max_n in [
        None,
        Some(-2.9),
        Some(-0.1),
        Some(0.0),
        Some(0.9),
        Some(1.0),
        Some(1.9),
        Some(2.1),
        Some(20.0),
    ] {
        for replacement in [
            b"Z".as_slice(),
            b"\0",
            b"\xff",
            b"a",
            b"%0",
            b"%%",
            b"%1",
            b"",
            b"longer",
        ] {
            assert_eq!(
                replace_bytes(vm.lua(), source, b"a", replacement, max_n),
                replace_bytes(&reference, source, b"a", replacement, max_n),
                "max_n {max_n:?}, replacement {replacement:?}"
            );
        }
    }
    // Preserve the current general matcher's exceptional-number policy as well;
    // changing numeric coercion is outside this optimization.
    for max_n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            replace_bytes(vm.lua(), source, b"a", b"Z", Some(max_n)),
            replace_bytes(vm.lua(), source, b"(a)", b"Z", Some(max_n)),
            "exceptional maximum {max_n}"
        );
    }
}

#[test]
fn literal_patterns_with_function_or_table_replacements_keep_native_results() {
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let reference = mlua::Lua::new();
    for replacement in [
        "function() return 'Z' end",
        "{a='Z'}",
        "function() return false end",
    ] {
        let source = format!(
            "return string.gsub(string.char(255)..'a'..string.char(0)..'a','a',{replacement})"
        );
        let (expected, expected_count): (LuaString, usize) =
            reference.load(&source).eval().unwrap();
        let (actual, actual_count): (LuaString, usize) = vm.lua().load(&source).eval().unwrap();
        assert_eq!(&*actual.as_bytes(), &*expected.as_bytes(), "{replacement}");
        assert_eq!(actual_count, expected_count, "{replacement}");
    }
}

#[test]
fn literal_gsub_keeps_the_result_cap_even_with_zero_replacements() {
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let subject = vm
        .lua()
        .create_string(vec![b'a'; 64 * 1024 * 1024 + 1])
        .unwrap();
    let function = string_function(&vm, "gsub");
    // A zero replacement maximum still returns the entire subject, so it must
    // enforce the output cap before allocating/copying that result.
    let error = function
        .call::<LuaString>((subject, "a", "b", 0))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("string.gsub: result too large (limit 64 MB)"),
        "{error}"
    );
}

#[test]
fn native_large_linear_match_completes_without_aggregate_complexity_rejection() {
    // Raw native invocation deliberately has no scheduler deadline: isolate the
    // complexity policy from the separate, unchanged 100 ms script budget.
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let subject = large_subject(&vm);
    let function = string_function(&vm, "match");
    let start = Instant::now();
    let result: Option<LuaString> = function.call((subject, "x")).unwrap();
    assert!(result.is_none());
    eprintln!("12 MB native match, no deadline: {:?}", start.elapsed());
}

#[test]
fn native_large_linear_gsub_completes_without_aggregate_complexity_rejection() {
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let subject = large_subject(&vm);
    let function = string_function(&vm, "gsub");
    let start = Instant::now();
    let (result, count): (LuaString, usize) = function.call((subject, "a", "b")).unwrap();
    assert_eq!(count, LARGE_INPUT);
    let bytes = result.as_bytes();
    assert_eq!(bytes.len(), LARGE_INPUT);
    assert!(bytes.iter().all(|&byte| byte == b'b'));
    eprintln!("12 MB native gsub, no deadline: {:?}", start.elapsed());
}

#[test]
fn native_large_gmatch_search_completes_without_aggregate_complexity_rejection() {
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let factory = string_function(&vm, "gmatch");
    let iterator: Function = factory.call((large_subject(&vm), "x")).unwrap();
    let result: Option<LuaString> = iterator.call(()).unwrap();
    assert!(result.is_none());
    let exhausted: Option<LuaString> = iterator.call(()).unwrap();
    assert!(exhausted.is_none());
}

#[test]
fn empty_matches_and_iterator_boundaries_keep_native_luau_results() {
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::new(Context::Server).unwrap();
    let reference = mlua::Lua::new();
    for source in [
        "local s,n=string.gsub('abc','','-') return s..':'..n",
        "local s,n=string.gsub('abc','a*','-') return s..':'..n",
        "local s,n=string.gsub('abc','','-',2) return s..':'..n",
        "local t={} for v in string.gmatch('abc','()') do t[#t+1]=v end return table.concat(t,',')",
        "local t={} for v in string.gmatch('abc','%a*') do t[#t+1]='['..v..']' end return table.concat(t,',')",
        "local t={} for v in string.gmatch('','()') do t[#t+1]=v end return table.concat(t,',')",
    ] {
        let expected: String = reference.load(source).eval().unwrap();
        let actual: String = vm.lua().load(source).eval().unwrap();
        assert_eq!(actual, expected, "{source}");
    }
}

#[test]
fn many_short_attempts_time_out_stickily_and_the_next_call_recovers() {
    common::real_limits_test(|| {
        let mut vm = ScriptVm::new(Context::Server).unwrap();
        vm.set_global(
            "subject",
            vm.lua().create_string(vec![b'a'; 32_000_000]).unwrap(),
        )
        .unwrap();
        let start = Instant::now();
        let error = vm
            .run_script(
                "ShortAttempts",
                "local ok=pcall(string.match,subject,'x?x?x?x?x?x?x?x?z') return ok",
            )
            .unwrap_err();
        assert!(
            error.message.contains("Script timeout"),
            "{}",
            error.message
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
        vm.run_script("Healthy", "assert(string.match('abc','b') == 'b')")
            .unwrap();
    });
}

#[test]
#[ignore = "manual acceptance probe: reports real budgeted outcomes separately from complexity tests"]
fn report_real_budgeted_twelve_mb_acceptance() {
    let mut accepted = 0;
    for name in ["match", "gsub"] {
        let vm = ScriptVm::new(Context::Server).unwrap();
        let subject = large_subject(&vm);
        let function = string_function(&vm, name);
        let start = Instant::now();
        let result = if name == "match" {
            vm.call_with_budget::<mlua::MultiValue>(&function, (subject, "x"))
        } else {
            vm.call_with_budget::<mlua::MultiValue>(&function, (subject, "a", "b"))
        };
        let elapsed = start.elapsed();
        match result {
            Ok(values) if name == "match" => {
                assert_eq!(values.len(), 1);
                assert!(matches!(values.front(), Some(mlua::Value::Nil)));
                accepted += 1;
                eprintln!("12 MB budgeted {name}: succeeded in {elapsed:?}");
            }
            Ok(values) => {
                let Some(mlua::Value::String(result)) = values.front() else {
                    panic!("gsub did not return its result string");
                };
                assert_eq!(result.as_bytes().len(), LARGE_INPUT);
                assert!(result.as_bytes().iter().all(|&byte| byte == b'b'));
                let count = match values.get(1) {
                    Some(mlua::Value::Number(count)) => *count,
                    Some(mlua::Value::Integer(count)) => *count as f64,
                    other => {
                        panic!("gsub did not return its numeric substitution count: {other:?}")
                    }
                };
                assert_eq!(count, LARGE_INPUT as f64);
                accepted += 1;
                eprintln!("12 MB budgeted {name}: succeeded in {elapsed:?}");
            }
            Err(error) => eprintln!("12 MB budgeted {name}: unmet in {elapsed:?}: {error}"),
        }
    }
    for (name, source) in [
        (
            "match",
            "local s=string.rep('a',12e6) assert(string.match(s,'x') == nil)",
        ),
        (
            "gsub",
            "local s=string.rep('a',12e6) local out,n=string.gsub(s,'a','b') assert(#out==12e6 and n==12e6)",
        ),
    ] {
        let mut vm = ScriptVm::new(Context::Server).unwrap();
        let start = Instant::now();
        let result = vm.run_script("OriginalRepro", source);
        let elapsed = start.elapsed();
        match result {
            Ok(()) => {
                accepted += 1;
                eprintln!("12 MB end-to-end budgeted {name}: succeeded in {elapsed:?}");
            }
            Err(error) => {
                eprintln!("12 MB end-to-end budgeted {name}: unmet in {elapsed:?}: {error}")
            }
        }
    }
    eprintln!("Actual budgeted acceptance: {accepted}/4");
    #[cfg(not(debug_assertions))]
    assert_eq!(accepted, 4, "optimized budgeted acceptance remains unmet");
}
