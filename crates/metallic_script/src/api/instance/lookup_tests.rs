//! What keeps lookup order unobservable while old tables and the registry both answer: names do
//! not clash on any class, so asking one before the other cannot change a script's answer.

use crate::api::bridge::BRIDGED_METHODS;
use crate::api::host_methods::NOT_MIGRATED_HOST_METHODS;
use metallic_core::classes::registry;

#[test]
fn a_bridged_method_leaves_the_list_once_the_registry_declares_it() {
    for (owner, methods) in BRIDGED_METHODS {
        let class = registry()
            .class_by_name(owner)
            .unwrap_or_else(|| panic!("{owner} is a class"));
        for method in *methods {
            assert!(
                class.member(method).is_none(),
                "{owner}.{method} is declared now: remove it from BRIDGED_METHODS and give it a \
                 body in class files"
            );
        }
    }
}

#[test]
fn a_method_is_listed_once() {
    let mut names: Vec<&str> = BRIDGED_METHODS
        .iter()
        .flat_map(|(_, methods)| methods.iter().copied())
        .chain(NOT_MIGRATED_HOST_METHODS.iter().map(|(_, method)| *method))
        .collect();
    let total = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), total);
}

#[test]
fn no_registry_member_of_a_class_shares_a_name_with_its_bridged_methods() {
    for class in registry().classes() {
        for member in &class.members {
            let name = member.name();
            let bridged = BRIDGED_METHODS
                .iter()
                .any(|(owner, methods)| methods.contains(&name) && class.is_a_named(owner));
            assert!(!bridged, "{}.{name} is also bridged", class.name());
        }
    }
}

#[test]
fn no_class_reaches_the_ids_of_the_host_object_signals() {
    use crate::api::signals::{LAST_INPUT_TYPE_CHANGED, TAG_ADDED, TAG_REMOVED};
    let lowest = [LAST_INPUT_TYPE_CHANGED, TAG_ADDED, TAG_REMOVED]
        .iter()
        .map(|id| usize::from(id.0))
        .min()
        .unwrap();
    for class in registry().classes() {
        assert!(class.members.len() < lowest, "{}", class.name());
    }
}

#[test]
fn every_not_migrated_host_method_is_declared_where_the_list_says() {
    for (owner, method) in NOT_MIGRATED_HOST_METHODS {
        let class = registry().class_by_name(owner).expect("a class");
        assert!(class.member(method).is_some(), "{owner}.{method}");
    }
}
