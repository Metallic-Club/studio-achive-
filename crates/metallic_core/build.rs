//! Class discovery: finds every class file under `src/classes/<group>/` and generates the module
//! tree plus the `ALL_CLASSES` and `ALL_ENUMS` lists that `classes::registry` resolves.
//!
//! A file takes part by having a line starting with `pub static CLASS:` or `pub static ENUMS:`.
//! Other files in a group folder (helpers) are compiled as plain modules. Only file paths and those
//! marker lines are read; Rust is never parsed beyond that. The folders are for people: inheritance comes only from
//! `ClassDesc::superclass`. Adding a class is adding one file.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{env, fmt::Write as _, fs};

const CLASS_MARKER: &str = "pub static CLASS:";
const ENUMS_MARKER: &str = "pub static ENUMS:";

/// One group folder: its Rust modules, and the folders inside it.
#[derive(Default)]
struct Group {
    /// Module name to absolute path, sorted so the generated file is deterministic.
    files: BTreeMap<String, String>,
    groups: BTreeMap<String, Group>,
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", dir.display()))
                .path()
        })
        .collect();
    paths.sort();
    paths
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_else(|| panic!("{} has no file name", path.display()))
        .to_string_lossy()
        .into_owned()
}

fn has_marker(source: &str, marker: &str) -> bool {
    source
        .lines()
        .any(|line| line.trim_start().starts_with(marker))
}

/// What the discovered files export, as paths from the generated module.
#[derive(Default)]
struct Exports {
    classes: Vec<String>,
    enums: Vec<String>,
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn scan(dir: &Path, module_path: &str, group: &mut Group, exports: &mut Exports) {
    for path in sorted_entries(dir) {
        let name = stem(&path);
        if path.is_dir() {
            let inner = group.groups.entry(name.clone()).or_default();
            scan(&path, &format!("{module_path}{name}::"), inner, exports);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let source = read(&path);
            if has_marker(&source, CLASS_MARKER) {
                exports.classes.push(format!("{module_path}{name}::CLASS"));
            }
            if has_marker(&source, ENUMS_MARKER) {
                exports.enums.push(format!("{module_path}{name}::ENUMS"));
            }
            let absolute = path.to_string_lossy().replace('\\', "/");
            group.files.insert(name, absolute);
        }
    }
}

fn write_modules(group: &Group, out: &mut String) {
    for (name, path) in &group.files {
        let _ = writeln!(out, "#[path = {path:?}]\npub mod {name};");
    }
    for (name, inner) in &group.groups {
        let _ = writeln!(out, "pub mod {name} {{");
        write_modules(inner, out);
        let _ = writeln!(out, "}}");
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("set by cargo"));
    let root = manifest.join("src").join("classes");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut group = Group::default();
    let mut exports = Exports::default();
    for path in sorted_entries(&root) {
        if path.is_dir() {
            let name = stem(&path);
            let inner = group.groups.entry(name.clone()).or_default();
            scan(&path, &format!("{name}::"), inner, &mut exports);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            // The top level holds the registry itself; a class here would be silently skipped.
            let source = read(&path);
            assert!(
                !has_marker(&source, CLASS_MARKER) && !has_marker(&source, ENUMS_MARKER),
                "{} declares a class or enums: those go in a group folder under src/classes/",
                path.display()
            );
        }
    }

    let mut code = String::new();
    write_modules(&group, &mut code);
    code.push_str("\npub static ALL_CLASSES: &[&crate::classes::ClassDesc] = &[\n");
    for class in &exports.classes {
        let _ = writeln!(code, "    &{class},");
    }
    code.push_str("];\n");
    code.push_str("\npub static ALL_ENUMS: &[&[crate::classes::EnumDesc]] = &[\n");
    for enums in &exports.enums {
        let _ = writeln!(code, "    {enums},");
    }
    code.push_str("];\n");
    fs::write(out_dir.join("classes_generated.rs"), code).expect("write classes_generated.rs");
}
