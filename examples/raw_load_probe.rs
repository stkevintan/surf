//! Narrow the Kontakt abort down to a single stage.
//!
//! Kontakt 8.14 dies with a libzmq `epoll.cpp:73` EBADF assert somewhere
//! between `LoadLibrary` and a usable plugin instance. `vst3-host` does four
//! things in a row there (load DLL → run the module initialiser → get the
//! factory → create the instance), and the existing probes only tell us that
//! the whole sequence fails. This one prints a line per stage so the log says
//! exactly which one aborts, without any of our hosting code in the way.
//!
//! ```powershell
//! cargo run --example raw_load_probe -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

use std::ffi::c_void;

use windows::Win32::Foundation::{FARPROC, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};
use windows::core::{HSTRING, PCSTR};

/// `InitDll`/`ExitDll` are the Windows VST3 module entry points, and
/// `GetPluginFactory` returns the `IPluginFactory` every host starts from.
type BoolFn = unsafe extern "system" fn() -> bool;
type FactoryFn = unsafe extern "system" fn() -> *mut c_void;

fn symbol(module: HMODULE, name: &str) -> FARPROC {
    let mut owned = name.as_bytes().to_vec();
    owned.push(0);
    // SAFETY: `owned` is NUL-terminated and outlives the call.
    unsafe { GetProcAddress(module, PCSTR(owned.as_ptr())) }
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: raw_load_probe <path-to.vst3>");

    // LOAD_WITH_ALTERED_SEARCH_PATH makes the plugin's own folder the first
    // place its dependencies are searched, which is what a host must do for
    // bundles that ship private DLLs next to the binary.
    let wide = HSTRING::from(path.as_str());
    println!("stage 1: LoadLibraryExW");
    // SAFETY: `wide` is a valid NUL-terminated wide path.
    let module = unsafe { LoadLibraryExW(&wide, None, LOAD_WITH_ALTERED_SEARCH_PATH) }?;
    println!("stage 1 ok: module={module:?}");

    println!("stage 2: InitDll");
    match symbol(module, "InitDll") {
        Some(ptr) => {
            // SAFETY: VST3 modules export InitDll as `bool()`.
            let init: BoolFn = unsafe { std::mem::transmute(ptr) };
            let ok = unsafe { init() };
            println!("stage 2 ok: InitDll -> {ok}");
        }
        None => println!("stage 2: no InitDll export (optional)"),
    }

    println!("stage 3: GetPluginFactory");
    let factory_ptr = symbol(module, "GetPluginFactory")
        .ok_or_else(|| anyhow::anyhow!("no GetPluginFactory export — not a VST3 module"))?;
    // SAFETY: VST3 modules export GetPluginFactory as `IPluginFactory*()`.
    let get_factory: FactoryFn = unsafe { std::mem::transmute(factory_ptr) };
    let factory = unsafe { get_factory() };
    println!("stage 3 ok: factory={factory:p}");

    println!("all stages survived; plugin never instantiated");
    Ok(())
}
