//! Generic functions.
//!
//! `fn id<T>(x: T) -> T` parsed and then went nowhere: the type parameter was
//! dropped, `T` was read as the name of an ordinary type, and the native
//! backend printed whatever bit pattern happened to be in the register. These
//! tests pin down both halves of the fix — the checker working out what each
//! type parameter stands for, and the specialisation pass giving every
//! instantiation its own copy of the function.

mod common;
use common::{assert_check_error, assert_checks, assert_output, assert_run_fails};

const ID: &str = "fn id<T>(x: T) -> T {\n    return x;\n}\n";

#[test]
fn one_generic_function_serves_several_types() {
    assert_output(
        &format!("{ID}fn main() {{ print(id(5)); print(id(\"hi\")); print(id(2.5)); print(id(true)); }}\n"),
        &["5", "hi", "2.5", "true"],
    );
}

#[test]
fn type_parameters_are_inferred_per_call() {
    let src = "fn fst<A, B>(a: A, b: B) -> A {\n    return a;\n}\n\
               fn main() { print(fst(1, \"x\")); print(fst(\"y\", 3)); }\n";
    assert_output(src, &["1", "y"]);
}

#[test]
fn one_type_parameter_cannot_stand_for_two_types() {
    let src = "fn same<T>(a: T, b: T) -> T {\n    return a;\n}\n\
               fn main() { print(same(1, \"x\")); }\n";
    assert_check_error(src, "expected `Int`, found `String`");
}

#[test]
fn a_generic_call_inside_a_generic_body_is_specialised_too() {
    // The inner `id(x)` is written once but means something different in each
    // copy of `twice`, which is why call sites are keyed by the function they
    // sit in and not by position alone.
    let src = "fn id<T>(x: T) -> T {\n    return x;\n}\n\
               fn twice<T>(x: T) -> T {\n    let a: T = id(x);\n    return id(a);\n}\n\
               fn main() { print(twice(\"deep\")); print(twice(4)); print(twice(1.5)); }\n";
    assert_output(src, &["deep", "4", "1.5"]);
}

#[test]
fn a_generic_function_may_recurse_at_the_same_type() {
    let src = "fn repeat<T>(x: T, n: int) -> T {\n\
               \x20   if n == 0 { return x; }\n\
               \x20   return repeat(x, n - 1);\n}\n\
               fn main() { print(repeat(\"same\", 3)); print(repeat(7, 2)); }\n";
    assert_output(src, &["same", "7"]);
}

#[test]
fn recursing_at_a_growing_type_is_rejected() {
    // Each call would need a copy taking one more layer of array than the last,
    // so there is no finite set of copies. Better to say so than to expand
    // until memory runs out.
    let src = "fn grow<T>(x: T, n: int) -> int {\n\
               \x20   if n == 0 { return 0; }\n\
               \x20   return grow([x], n - 1);\n}\n\
               fn main() { print(grow(1, 40)); }\n";
    assert_run_fails(src, "new copy of the function every time");
}

#[test]
fn a_type_parameter_that_nothing_pins_down_is_an_error() {
    let src = "fn nothing<T>(n: int) -> int {\n    return n;\n}\n\
               fn main() { print(nothing(1)); }\n";
    assert_check_error(src, "T");
}

#[test]
fn generic_arguments_can_be_structs_and_enums() {
    let src = "struct Point { x: int, y: int }\n\
               fn id<T>(x: T) -> T {\n    return x;\n}\n\
               fn main() { let p = Point { x: 1, y: 2 }; print(id(p).y); print(id(Some(9))); }\n";
    assert_output(src, &["2", "Some(9)"]);
}

#[test]
fn a_generic_declaration_on_its_own_still_checks() {
    assert_checks(&format!("{ID}fn main() {{ print(1); }}\n"));
}
