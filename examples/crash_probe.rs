//! Load a VST3 while a vectored exception handler reports where it crashes.
//!
//! Kontakt 8.14 aborts before `createInstance` returns on this machine; the
//! process exits with `0xC0000005` and no dump. A VEH runs before any SEH, so
//! it prints the faulting module (Kontakt itself? a graphics driver? the VC++
//! runtime?) without needing a debugger or admin rights.
//!
//! ```powershell
//! cargo run --example crash_probe -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

#[cfg(windows)]
mod probe {
    use surf::plugin::backends::vst3::VstPlugin;
    use windows::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
    };
    use windows::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    use windows::core::PCWSTR;

    unsafe extern "system" fn report(info: *mut EXCEPTION_POINTERS) -> i32 {
        unsafe {
            let record = (*info).ExceptionRecord;
            let code = (*record).ExceptionCode.0;
            let address = (*record).ExceptionAddress;

            let mut module = [0u16; 512];
            let mut len = 0;
            let mut handle = Default::default();
            if GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                PCWSTR(address as *const u16),
                &mut handle,
            )
            .is_ok()
            {
                len = GetModuleFileNameW(Some(handle), &mut module);
            }
            let name = String::from_utf16_lossy(&module[..len as usize]);
            eprintln!(
                "VEH: code=0x{code:08x} address={address:p} module={}",
                if name.is_empty() { "<unknown>" } else { &name }
            );
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    pub fn main() -> anyhow::Result<()> {
        let path = std::env::args()
            .nth(1)
            .expect("usage: crash_probe <path-to.vst3>");
        unsafe {
            AddVectoredExceptionHandler(1, Some(report));
        }
        println!("loading {path}");
        let plugin = VstPlugin::load(&path, 48_000)?;
        println!("name: {}", plugin.name);
        println!("has_editor: {}", plugin.has_editor());
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    probe::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("this probe is Windows-only");
}
