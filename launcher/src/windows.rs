use serde_json::{Value, json};
use std::ffi::c_void;
use std::io::{Error, Result};
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::*;

const MAX_ABI_INPUT: usize = 1_048_576;
const MAX_ABI_OUTPUT: usize = 20 * 1024 * 1024;
const ABI_STATUS_OK: i32 = 0;
const ABI_STATUS_INPUT_TOO_LARGE: i32 = 2;

type Check = unsafe extern "C" fn(*mut c_void) -> bool;
type Reply = unsafe extern "C" fn(*mut c_void, *const u8, usize);
type Call = unsafe extern "C" fn(*const u8, usize, Check, Reply, *mut c_void) -> i32;
type Close = unsafe extern "C" fn();

struct ModuleGuard(HMODULE);
impl Drop for ModuleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                FreeLibrary(self.0);
            }
        }
    }
}

struct Engine {
    _handle: HMODULE,
    call: Call,
    close: Close,
}
impl Engine {
    fn load() -> Result<Self> {
        let path = std::env::current_exe()?.with_file_name("autogui_runtime.dll");
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            LoadLibraryExW(
                path.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if handle.is_null() {
            return Err(Error::other(format!(
                "Cannot load adjacent autogui_runtime.dll: {}. Keep EXE and DLL together.",
                Error::last_os_error()
            )));
        }
        let mut guard = ModuleGuard(handle);
        let abi = unsafe { GetProcAddress(guard.0, c"autogui_abi_version".as_ptr().cast()) }
            .ok_or_else(|| Error::other("Runtime ABI export missing"))?;
        let abi: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(abi) };
        if unsafe { abi() } != 1 {
            return Err(Error::other("Runtime ABI version mismatch"));
        }
        let call = unsafe { GetProcAddress(guard.0, c"autogui_call_json".as_ptr().cast()) }
            .ok_or_else(|| Error::other("Runtime call export missing"))?;
        let close = unsafe { GetProcAddress(guard.0, c"autogui_session_close".as_ptr().cast()) }
            .ok_or_else(|| Error::other("Runtime close export missing"))?;
        guard.0 = std::ptr::null_mut();
        Ok(Self {
            _handle: handle,
            call: unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, Call>(call)
            },
            close: unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, Close>(close)
            },
        })
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { (self.close)() };
        // Keep the module mapped until process exit: Rust TLS destructors and
        // any completing pause-output thread still reference runtime code.
    }
}
#[derive(Default)]
pub(super) struct LazyEngine {
    engine: Option<Engine>,
}
struct Context<'a> {
    check: &'a dyn Fn() -> Result<()>,
    output: Option<Vec<u8>>,
    overflow: Option<usize>,
}
unsafe extern "C" fn check(context: *mut c_void) -> bool {
    let context = unsafe { &*(context as *const Context<'_>) };
    (context.check)().is_ok()
}
unsafe extern "C" fn reply(context: *mut c_void, bytes: *const u8, len: usize) {
    let context = unsafe { &mut *(context as *mut Context<'_>) };
    if bytes.is_null() {
        return;
    }
    if len > MAX_ABI_OUTPUT {
        context.output = None;
        context.overflow = Some(len);
        return;
    }
    context.overflow = None;
    context.output = Some(unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec());
}
impl super::mcp::Handler for LazyEngine {
    fn call(
        &mut self,
        name: &str,
        args: Value,
        check_cancelled: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        if self.engine.is_none() {
            self.engine = Some(Engine::load()?);
        }
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| Error::other("runtime unavailable"))?;
        let input = serde_json::to_vec(&json!({"name":name,"arguments":args}))?;
        if input.len() > MAX_ABI_INPUT {
            return Err(Error::other(format!(
                "Runtime input exceeds 1 MiB ABI limit ({} bytes)",
                input.len()
            )));
        }
        let mut context = Context {
            check: check_cancelled,
            output: None,
            overflow: None,
        };
        let status = unsafe {
            (engine.call)(
                input.as_ptr(),
                input.len(),
                check,
                reply,
                (&mut context as *mut Context<'_>).cast(),
            )
        };
        match status {
            ABI_STATUS_OK => {}
            ABI_STATUS_INPUT_TOO_LARGE => {
                return Err(Error::other("Runtime input exceeds 1 MiB ABI limit"));
            }
            _ => return Err(Error::other("Runtime call failed at ABI boundary")),
        }
        if let Some(len) = context.overflow {
            return Err(Error::other(format!(
                "Runtime response exceeds 20 MiB ABI limit ({len} bytes)"
            )));
        }
        serde_json::from_slice(
            &context
                .output
                .ok_or_else(|| Error::other("Runtime returned no response"))?,
        )
        .map_err(Error::other)
    }
}
pub(super) fn run_cli() -> Result<()> {
    let engine = Engine::load()?;
    let entry = unsafe { GetProcAddress(engine._handle, c"autogui_cli_main".as_ptr().cast()) }
        .ok_or_else(|| Error::other("Runtime CLI export missing"))?;
    let entry: unsafe extern "C" fn() -> i32 = unsafe { std::mem::transmute(entry) };
    if unsafe { entry() } == 0 {
        Ok(())
    } else {
        Err(Error::other("Runtime command failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_records_valid_output_and_flags_overflow() {
        let noop = || Ok(());
        let mut ctx = Context {
            check: &noop,
            output: None,
            overflow: None,
        };
        let ptr = (&mut ctx as *mut Context<'_>).cast();
        let small = b"{\"ok\":true}";
        unsafe { reply(ptr, small.as_ptr(), small.len()) };
        assert_eq!(ctx.output.as_deref(), Some(small.as_slice()));
        assert_eq!(ctx.overflow, None);

        unsafe { reply(ptr, small.as_ptr(), MAX_ABI_OUTPUT + 1) };
        assert_eq!(ctx.output, None);
        assert_eq!(ctx.overflow, Some(MAX_ABI_OUTPUT + 1));
    }
}
