//! `tumbler-cli`: Tumbler's command line and MCP server (doc/cli-plan.html).
//!
//! Packaging-spike stub: proves a second binary builds, bundles and leaves
//! the GUI as the default `cargo run` target. The real entry point replaces
//! this in phase 3.

fn main() {
    println!("tumbler-cli {}", env!("CARGO_PKG_VERSION"));
}
