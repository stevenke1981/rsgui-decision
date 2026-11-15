mod cli;
#[path = "../../examples/support/mcp.rs"]
mod mcp;
#[cfg(windows)]
mod windows;

fn main() {
    cli::run("rsgui-decision");
}
