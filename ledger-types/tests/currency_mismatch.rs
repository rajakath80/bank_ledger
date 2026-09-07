//! `M0` "done when": a `trybuild` compile-fail test proves USD + EUR does
//! not compile.

#[test]
fn currency_mismatch_does_not_compile() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/currency_mismatch.rs");
}
