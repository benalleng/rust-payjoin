use std::env;

fn main() {
    let mut source_code_path = "target/release/libpayjoin_ffi".to_owned();
    if env::consts::OS == "linux" {
        source_code_path.push_str(".so");
    } else if env::consts::OS == "macos" {
        source_code_path.push_str(".dylib");
    } else {
        source_code_path.push_str(".dll");
    };
    #[cfg(feature = "uniffi")]
    uniffi::uniffi_bindgen_main();
    #[cfg(feature = "uniffi")]
    uniffi_dart::gen::generate_dart_bindings(
        "src/payjoin_ffi.udl".into(),
        None,
        Some("dart/lib".into()),
        source_code_path.as_str().into(),
        true,
    )
    .unwrap();
}
