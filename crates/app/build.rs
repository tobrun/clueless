//! Link flag so the binary finds the Swift runtime libraries at run time
//! (ScreenCaptureKit's async bridges pull in Swift symbols).

fn main() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
