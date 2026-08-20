fn main() {
    // Only the GUI binary needs Tauri's build step (Windows resources, the
    // embedded icon, config checks). The headless binary and `cargo test` on a
    // machine with no webview toolchain must keep working without it -- that
    // property is why the protocol is testable in CI at all.
    if std::env::var_os("CARGO_FEATURE_GUI").is_some() {
        tauri_build::build();
    }
}
