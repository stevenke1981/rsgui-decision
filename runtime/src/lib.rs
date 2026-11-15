//! Versioned C boundary: no Rust allocator or unwinding crosses the DLL boundary.
#[path = "../../examples/app_control.rs"]
mod app;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::ffi::c_void;

thread_local! { static SESSION: RefCell<app::mcp_tools::Session> = RefCell::new(Default::default()); }
type Check = unsafe extern "C" fn(*mut c_void) -> bool;
type Reply = unsafe extern "C" fn(*mut c_void, *const u8, usize);

#[unsafe(no_mangle)]
pub extern "C" fn autogui_abi_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn autogui_cli_main() -> i32 {
    if std::panic::catch_unwind(app::main).is_ok() {
        0
    } else {
        1
    }
}

/// # Safety
/// Input must reference len readable bytes. Callback/context must remain valid
/// during the synchronous call. Reply bytes are borrowed only inside callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn autogui_call_json(
    input: *const u8,
    len: usize,
    check: Check,
    reply: Reply,
    context: *mut c_void,
) -> i32 {
    if input.is_null() {
        return 1;
    }
    if len > 1_048_576 {
        return 2;
    }
    let result = std::panic::catch_unwind(|| {
        let bytes = unsafe { std::slice::from_raw_parts(input, len) };
        let mut request: Value = serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
        let Some(map) = request.as_object_mut() else {
            return Err(std::io::Error::other("invalid runtime request"));
        };
        let name = map
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| std::io::Error::other("missing name"))?
            .to_owned();
        let args = map.remove("arguments").unwrap_or(Value::Null);
        let check = || {
            if unsafe { check(context) } {
                Ok(())
            } else {
                Err(autogui::Error::Input(
                    "request cancelled or client disconnected".into(),
                ))
            }
        };
        let value = SESSION.with(|s| s.borrow_mut().call(&name, args, &check));
        let value = match value {
            Ok(v) => v,
            Err(e) => json!({"content":[{"type":"text","text":e.to_string()}],"isError":true}),
        };
        let bytes = serde_json::to_vec(&value).map_err(std::io::Error::other)?;
        unsafe { reply(context, bytes.as_ptr(), bytes.len()) };
        Ok::<(), std::io::Error>(())
    });
    if matches!(result, Ok(Ok(()))) { 0 } else { 1 }
}

#[unsafe(no_mangle)]
pub extern "C" fn autogui_session_close() {
    let _ = std::panic::catch_unwind(|| SESSION.with(|s| *s.borrow_mut() = Default::default()));
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn always_ok(_: *mut c_void) -> bool {
        true
    }

    unsafe extern "C" fn capture_reply(ctx: *mut c_void, bytes: *const u8, len: usize) {
        let out = unsafe { &mut *(ctx as *mut Vec<u8>) };
        *out = unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec();
    }

    #[test]
    fn abi_version_and_size_status_codes() {
        assert_eq!(autogui_abi_version(), 1);
        let mut out = Vec::<u8>::new();
        let ctx = (&mut out as *mut Vec<u8>).cast();
        assert_eq!(
            unsafe { autogui_call_json(std::ptr::null(), 0, always_ok, capture_reply, ctx) },
            1
        );
        let payload = b"{}";
        assert_eq!(
            unsafe {
                autogui_call_json(payload.as_ptr(), 1_048_577, always_ok, capture_reply, ctx)
            },
            2
        );
        let req = br#"{"name":"session_status","arguments":{}}"#;
        assert_eq!(
            unsafe { autogui_call_json(req.as_ptr(), req.len(), always_ok, capture_reply, ctx) },
            0
        );
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["isError"], false);
        autogui_session_close();
    }
}
