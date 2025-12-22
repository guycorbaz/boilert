//! Build script for the boilert project.
//! This script runs during compilation to handle Slint UI file compilation.

fn main() {
    // Compile the main Slint UI entry point.
    // This generates the Rust code corresponding to the .slint files.
    slint_build::compile("ui/app-window.slint").expect("Slint build failed");
}
