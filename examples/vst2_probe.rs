//! Minimal VST2 host probe:
//! `cargo run --example vst2_probe -- "<path>\Kontakt 8 Portable.dll"`

#[cfg(windows)]
mod probe {
    use std::sync::{Arc, Mutex};

    use vst::host::{Host, PluginLoader};
    use vst::plugin::Plugin;
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
    };

    struct ProbeHost {
        version: isize,
        vendor: String,
        product: String,
    }

    impl Host for ProbeHost {
        fn get_info(&self) -> (isize, String, String) {
            (self.version, self.vendor.clone(), self.product.clone())
        }

        fn get_block_size(&self) -> isize {
            64
        }

        fn get_sample_rate(&self) -> f32 {
            48_000.0
        }

        fn get_directory(&self) -> Option<String> {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        }

        fn can_do(&self, query: &str) -> bool {
            matches!(
                query,
                "send_vst_events" | "receive_vst_events" | "receive_sysex"
            )
        }

        fn get_current_process_level(&self) -> isize {
            1
        }

        fn get_time_info(&self, _mask: i32) -> Option<vst::api::TimeInfo> {
            let mut info = vst::api::TimeInfo::default();
            info.sample_rate = 48_000.0;
            info.tempo = 120.0;
            Some(info)
        }
    }

    pub fn main() -> anyhow::Result<()> {
        let path = std::env::args()
            .nth(1)
            .expect("usage: vst2_probe <plugin.dll>");
        // Set the current directory to the portable x64 directory so the wrapper
        // can find ktp871.dll, just like Cubase does.
        if let Some(dir) = std::path::Path::new(&path).parent() {
            let search = if dir
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("VST"))
            {
                dir.parent().unwrap_or(dir)
            } else {
                dir
            };
            let _ = std::env::set_current_dir(search);
        }

        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let class = HSTRING::from("STATIC");
            let title = HSTRING::from("Surf VST2 host");
            let _window = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &class,
                &title,
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                320,
                200,
                None,
                None,
                None,
                None,
            )?;
        }

        let host = Arc::new(Mutex::new(ProbeHost {
            version: std::env::var("HOST_VERSION")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2400),
            vendor: std::env::var("HOST_VENDOR")
                .unwrap_or_else(|_| "Steinberg Media Technologies GmbH".to_string()),
            product: std::env::var("HOST_PRODUCT").unwrap_or_else(|_| "Cubase 15".to_string()),
        }));
        let mut loader = PluginLoader::load(std::path::Path::new(&path), host)?;
        let mut instance = loader.instance().map_err(|e| anyhow::anyhow!("{e}"))?;
        let info = instance.get_info();
        println!(
            "name={} vendor={} inputs={} outputs={} params={}",
            info.name, info.vendor, info.inputs, info.outputs, info.parameters
        );
        instance.init();
        instance.set_sample_rate(48_000.0);
        instance.set_block_size(64);
        instance.resume();
        println!("editor={}", instance.get_editor().is_some());
        println!("bank_bytes={}", instance.get_parameter_object().get_bank_data().len());
        instance.suspend();
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
