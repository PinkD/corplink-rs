use std::env;
use std::path::PathBuf;
use bindgen::callbacks::{IntKind, ParseCallbacks};


#[derive(Debug)]
struct DefineParser;

impl ParseCallbacks for DefineParser {
    fn int_macro(&self, _name: &str, value: i64) -> Option<IntKind> {
        if value >= i32::MIN as i64 && value <= i32::MAX as i64 {
            Some(IntKind::I32)
        } else {
            None
        }
    }
}

fn main() {
    // Tell cargo to look for shared libraries in the specified directory
    println!("cargo:rustc-link-search=./libwg");

    // Tell cargo to tell rustc to link the shared library.
    println!("cargo:rustc-link-lib=wg");

    // Tell cargo to invalidate the built crate whenever the wrapper changes
    println!("cargo:rerun-if-changed=./libwg/libwg.h");

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // The input header we would like to generate
        // bindings for.
        .header("./libwg/libwg.h")
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed.
        .parse_callbacks(Box::new(
            bindgen::CargoCallbacks,
        ))
        // parse number define macro as i32 instead of u32
        .parse_callbacks(Box::new(
            DefineParser,
        ))
        // Finish the builder and generate the bindings.
        .generate()
        // Unwrap the Result and panic on failure.
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}