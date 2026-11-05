//! The VST2 editor's parent window on Windows: a plain Win32 top-level window.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use vst::editor::Editor;
use vst::host::PluginInstance;
use vst::plugin::Plugin;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, HCURSOR, HICON, IsWindow,
    RegisterClassW, SW_SHOW, SetWindowPos, ShowWindow, WM_CLOSE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::HSTRING;

pub fn open(plugin: &mut PluginInstance, name: &str) -> Result<Vst2EditorWindow> {
    let mut editor = plugin.get_editor().context("VST2 plugin has no editor")?;
    let window = NativeWindow::new(name)?;
    let parent = window.hwnd.0 as *mut c_void;
    if !editor.open(parent) {
        anyhow::bail!("VST2 editor refused to open");
    }
    let (w, h) = editor.size();
    window.resize(w, h);
    window.show();
    Ok(Vst2EditorWindow { window, editor })
}

/// Editor windows whose close button was clicked, keyed by `HWND` as `usize`.
///
/// `DefWindowProcW` would answer `WM_CLOSE` by destroying the window while the plugin's
/// editor is still attached to it. The window procedure records the request instead and the
/// host closes the editor, then the window, in that order.
fn close_requests() -> &'static Mutex<HashSet<usize>> {
    static REQUESTS: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    REQUESTS.get_or_init(Mutex::default)
}

unsafe extern "system" fn vst2_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    if msg == WM_CLOSE {
        if let Ok(mut requests) = close_requests().lock() {
            requests.insert(hwnd.0 as usize);
        }
        return windows::Win32::Foundation::LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

struct NativeWindow {
    hwnd: HWND,
    class: HSTRING,
}

impl NativeWindow {
    fn new(title: &str) -> Result<Self> {
        let class = HSTRING::from("SurfVst2Editor");
        let instance = unsafe { GetModuleHandleW(None)? };
        let wc = WNDCLASSW {
            style: Default::default(),
            lpfnWndProc: Some(vst2_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance.into(),
            hIcon: HICON::default(),
            hCursor: HCURSOR::default(),
            hbrBackground: HBRUSH::default(),
            lpszMenuName: windows::core::PCWSTR::null(),
            lpszClassName: windows::core::PCWSTR(class.as_ptr()),
        };
        unsafe {
            let _ = RegisterClassW(&wc);
        }
        let title = HSTRING::from(title);
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                &class,
                &title,
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                900,
                700,
                None,
                None,
                Some(instance.into()),
                None,
            )?
        };
        Ok(Self { hwnd, class })
    }

    fn resize(&self, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, 0, 0, width, height, Default::default());
        }
    }

    fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        if let Ok(mut requests) = close_requests().lock() {
            requests.remove(&(self.hwnd.0 as usize));
        }
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

pub struct Vst2EditorWindow {
    window: NativeWindow,
    editor: Box<dyn Editor>,
}

impl Vst2EditorWindow {
    pub fn idle(&mut self) {
        self.editor.idle();
    }

    /// Whether the user clicked the window's close button. The host should drop this
    /// window when it reports `true`.
    pub fn closed_by_user(&self) -> bool {
        close_requests()
            .lock()
            .is_ok_and(|requests| requests.contains(&(self.window.hwnd.0 as usize)))
    }
}

impl Drop for Vst2EditorWindow {
    fn drop(&mut self) {
        if self.editor.is_open() {
            self.editor.close();
        }
        let _ = &self.window.class;
    }
}
