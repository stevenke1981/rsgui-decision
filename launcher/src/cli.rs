pub(crate) fn version_text(program: &str) -> String {
    format!("{program} {}", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn help_text(program: &str) -> String {
    format!(
        "AutoGui Windows control\n  \
{program} mcp\n  \
{program} display list\n  \
{program} display capture <device name> <output.png>\n  \
{program} display move <device name> <unique title>\n  \
{program} list [title]\n  \
{program} capture <title> <output.png>\n  \
{program} locate|click <title> <template> [confidence|exact] [timeout] [scale]\n  \
{program} type <title> <text>\n  \
{program} window <inspect|activate|minimize|maximize|restore|show|hide|move|resize|close> <title> [x y|width height]\n  \
{program} screensaver status|pause [1..86400 seconds]|resume|stop\n\n\
Runtime: autogui_runtime.dll beside this EXE; loaded on demand.\n\
MCP initialization/tool listing do not load the DLL. Full image formats retained.\n\
Default: primary monitor. capture/locate/click accept a final --display <device name>.\n\
Explicit display coordinates use Windows desktop physical pixels (may be negative).\n\
Virtual displays require an optional IDD driver; all displays share input/focus.\n\
Primary-monitor corner fail-safe remains enabled.\n\
Titles must match one window. close requests WM_CLOSE; it does not confirm exit.\n\n\
Rust decision integration: decision_observe, decision_evaluate, decision_act, decision_step, decision_status (MCP).\n\
One stored action + adaptive wait + fresh capture per step.\n\
RSGUI_DECISION_MODE=local (default), auto, or gemini.\n\
Remote modes send selected window pixels to Google; explicitly configure GEMINI_API_KEY.\n\
--version works without loading the runtime."
    )
}

pub(crate) fn run(program: &str) {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        println!("{}", version_text(program));
        return;
    }
    if args.is_empty() || args == ["--help"] {
        println!("{}", help_text(program));
        return;
    }
    #[cfg(windows)]
    let result = if args == ["mcp"] {
        crate::mcp::run(crate::windows::LazyEngine::default())
    } else {
        crate::windows::run_cli()
    };
    #[cfg(not(windows))]
    let result: std::io::Result<()> = Err(std::io::Error::other("Windows runtime required"));
    if let Err(e) = result {
        eprintln!("AutoGui: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_help_match_program_name() {
        assert_eq!(
            version_text("autogui-control"),
            format!("autogui-control {}", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            version_text("rsgui-decision"),
            format!("rsgui-decision {}", env!("CARGO_PKG_VERSION"))
        );
        let help = help_text("rsgui-decision");
        assert!(help.contains("rsgui-decision mcp"));
        assert!(!help.contains("autogui-control"));
    }
}
