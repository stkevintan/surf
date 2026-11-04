//! Does initialising Winsock before loading fix Kontakt's ZeroMQ abort?
//!
//! Kontakt 8.14 dies with
//!
//! ```text
//! Bad file descriptor (D:\NIBuild\3rdparty\zeromq-v4.3.5-R2\src\epoll.cpp:73)
//! ```
//!
//! In libzmq 4.3.5 that line is the `errno_assert (epoll_fd != -1)` at the end
//! of the `epoll_t` constructor, and "Bad file descriptor" is `EBADF`.  On
//! Windows libzmq's epoll is wepoll, which is built on an I/O completion port
//! over Winsock handles — so it only works once *something* in the process has
//! called `WSAStartup`.  Kontakt's standalone `.exe` and every DAW do that
//! long before any plugin loads; a minimal Rust host never touches a socket,
//! so Winsock is still uninitialised and wepoll's `epoll_create` fails.
//!
//! This probe calls `WSAStartup` first and then loads the plugin normally.
//!
//! ```powershell
//! cargo run --example winsock_probe -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

#[cfg(windows)]
mod probe {
    use surf::plugin::backends::vst3::VstPlugin;

    /// `WSADATA` is 408 bytes on x64; we only need somewhere for it to be written.
    #[repr(C)]
    struct WsaData([u8; 408]);

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn WSAStartup(version: u16, data: *mut WsaData) -> i32;
    }

    pub fn main() -> anyhow::Result<()> {
        let path = std::env::args()
            .nth(1)
            .expect("usage: winsock_probe <path-to.vst3>");

        if std::env::var_os("SURF_NO_WSA").is_some() {
            println!("WSAStartup skipped (SURF_NO_WSA set)");
        } else {
            let mut data = WsaData([0; 408]);
            // SAFETY: `data` is a live, correctly sized WSADATA buffer.  2.2 is the
            // version every modern Windows process requests.
            let rc = unsafe { WSAStartup(0x0202, &mut data) };
            println!("WSAStartup(2.2) -> {rc}");
            anyhow::ensure!(rc == 0, "WSAStartup failed with {rc}");
        }

        println!("loading {path}");
        let plugin = VstPlugin::load(&path, 48_000)?;
        println!("name: {}", plugin.name);
        println!("has_editor: {}", plugin.has_editor());
        let state = plugin.state()?;
        println!("state: {} bytes", state.len());
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
