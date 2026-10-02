//! Test binaries of this crate link Swift runtime libraries (through
//! `screencapturekit`); add /usr/lib/swift to the rpath so dyld finds them.

fn main() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    println!("cargo:rerun-if-changed=build.rs");
}
