use crate::model::{
    ActivationMode, AutoHide, ClickFeedback, PresentationPreset, Settings, Shortcut, render,
};
use crate::startup;
use crate::updater::Updates;
use std::{
    cell::{Cell, RefCell},
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
        UI::{
            Controls::{Dialogs::*, *},
            HiDpi::*,
            Input::KeyboardAndMouse::*,
            Shell::*,
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

const ID_ENABLED: u16 = 101;
const ID_DIAMETER: u16 = 102;
const ID_OPACITY: u16 = 103;
const ID_COLOR: u16 = 104;
const ID_HIDE: u16 = 105;
const ID_EXIT: u16 = 106;
const ID_OPEN: u16 = 107;
const ID_SIZE_LABEL: u16 = 108;
const ID_OPACITY_LABEL: u16 = 109;
const ID_STATUS: u16 = 110;
const ID_CLICK_COLORS: u16 = 120;
const ID_PRESET_SLIDES: u16 = 111;
const ID_PRESET_DEMO: u16 = 112;
const ID_AUTO_HIDE: u16 = 113;
const ID_SCALE_WITH_DPI: u16 = 114;
const ID_START_MINIMIZED: u16 = 115;
const ID_LAUNCH_AT_LOGIN: u16 = 116;
const ID_SHORTCUT: u16 = 117;
const ID_APPLY_SHORTCUT: u16 = 118;
const ID_HOLD: u16 = 119;
const ID_OUTLINE: u16 = 121;
const ID_UPDATES: u16 = 122;
const ID_UPDATE_AUTO: u16 = 130;
const ID_UPDATE_STATUS: u16 = 131;
const ID_UPDATE_CHECK: u16 = 132;
const ID_UPDATE_INSTALL: u16 = 133;
const FRAME_TIMER: usize = 1;
const SAVE_TIMER: usize = 2;
const UPDATE_TIMER: usize = 3;
const TRAY_MESSAGE: u32 = WM_APP + 1;
const TBM_GETPOS: u32 = WM_USER;
const HALO_CLASS: *const u16 = w!("CursorHighlighter.Rust.Halo");
const SETTINGS_CLASS: *const u16 = w!("CursorHighlighter.Rust.Settings");
const UPDATES_CLASS: *const u16 = w!("CursorHighlighter.Rust.Updates");
// MAKEINTRESOURCEW encodes an integer resource ID, not an address to dereference.
const ICON_RESOURCE: *const u16 = std::ptr::without_provenance(1);

thread_local! {
    // The pinned Box<App> lives through the entire native message loop. Callbacks
    // use shared references; RefCell borrows never span a reentrant Win32 call.
    static CURRENT: Cell<*const App> = const { Cell::new(null()) };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn os_error(operation: &str) -> String {
    format!("{operation}: {}", std::io::Error::last_os_error())
}

fn check(value: i32, operation: &str) -> Result<(), String> {
    if value == 0 {
        Err(os_error(operation))
    } else {
        Ok(())
    }
}

fn chord_held(shortcut: &Shortcut, down: impl Fn(i32) -> bool) -> bool {
    down(shortcut.key as i32)
        && (!shortcut.control || down(VK_CONTROL as i32))
        && (!shortcut.alt || down(VK_MENU as i32))
        && (!shortcut.shift || down(VK_SHIFT as i32))
        && (!shortcut.win || down(VK_LWIN as i32) || down(VK_RWIN as i32))
}

fn halo_position(cursor: POINT, size: i32) -> POINT {
    POINT {
        x: cursor.x - size / 2,
        y: cursor.y - size / 2,
    }
}

fn halo_alpha(settings: &Settings, auto_hide: &mut AutoHide, now: u64) -> u8 {
    if settings.auto_hide && settings.activation_mode == ActivationMode::Toggle {
        auto_hide.alpha(now)
    } else {
        255
    }
}

fn with_app<R>(callback: impl FnOnce(&App) -> R) -> Option<R> {
    CURRENT.with(|current| {
        let pointer = current.get();
        if pointer.is_null() {
            None
        } else {
            // CURRENT is set only while the stable Box<App> is alive.
            Some(callback(unsafe { &*pointer }))
        }
    })
}

pub fn report_error(error: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(error).as_ptr(),
            w!("Cursor Highlighter"),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[derive(Clone, Copy)]
struct Control {
    window: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

struct App {
    instance: HINSTANCE,
    window: Cell<HWND>,
    halo: Cell<HWND>,
    dpi_probe: Cell<HWND>,
    hook: Cell<HHOOK>,
    keyboard_hook: Cell<HHOOK>,
    font: Cell<HFONT>,
    controls: RefCell<Vec<Control>>,
    settings: RefCell<Settings>,
    settings_path: PathBuf,
    click: RefCell<ClickFeedback>,
    auto_hide: RefCell<AutoHide>,
    cursor_position: Cell<Option<(i32, i32)>>,
    start: Instant,
    rendered: Cell<f32>,
    rendered_colors: Cell<[f32; 2]>,
    rendered_diameter: Cell<u32>,
    rendered_alpha: Cell<u8>,
    appearance_dirty: Cell<bool>,
    dirty: Cell<bool>,
    tray_added: Cell<bool>,
    hotkey_id: Cell<i32>,
    hold_active: Cell<bool>,
    fatal: RefCell<Option<String>>,
    dpi: Cell<u32>,
    taskbar_message: Cell<u32>,
    launch_at_login: Cell<bool>,
    updates_window: Cell<HWND>,
    updates_font: Cell<HFONT>,
    updates_controls: RefCell<Vec<Control>>,
    updates: RefCell<Updates>,
    update_summary: RefCell<String>,
    update_dialog_text: RefCell<String>,
}

impl App {
    fn new(settings: Settings, settings_path: PathBuf) -> Self {
        Self {
            instance: unsafe { GetModuleHandleW(null()) },
            window: Cell::new(null_mut()),
            halo: Cell::new(null_mut()),
            dpi_probe: Cell::new(null_mut()),
            hook: Cell::new(null_mut()),
            keyboard_hook: Cell::new(null_mut()),
            font: Cell::new(null_mut()),
            controls: RefCell::new(Vec::new()),
            settings: RefCell::new(settings),
            settings_path,
            click: RefCell::new(ClickFeedback::default()),
            auto_hide: RefCell::new(AutoHide::new(0)),
            cursor_position: Cell::new(None),
            start: Instant::now(),
            rendered: Cell::new(-1.0),
            rendered_colors: Cell::new([0.0; 2]),
            rendered_diameter: Cell::new(0),
            rendered_alpha: Cell::new(255),
            appearance_dirty: Cell::new(true),
            dirty: Cell::new(false),
            tray_added: Cell::new(false),
            hotkey_id: Cell::new(0),
            hold_active: Cell::new(false),
            fatal: RefCell::new(None),
            dpi: Cell::new(96),
            taskbar_message: Cell::new(0),
            launch_at_login: Cell::new(false),
            updates_window: Cell::new(null_mut()),
            updates_font: Cell::new(null_mut()),
            updates_controls: RefCell::new(Vec::new()),
            updates: RefCell::new(Updates::new()),
            update_summary: RefCell::new(String::new()),
            update_dialog_text: RefCell::new(String::new()),
        }
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn setup(&self, visible: bool) -> Result<(), String> {
        unsafe {
            let common = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
            };
            check(InitCommonControlsEx(&common), "Initialize native controls")?;
            let icon = LoadIconW(self.instance, ICON_RESOURCE);
            if icon.is_null() {
                return Err(os_error("Load custom icon"));
            }
            for (name, procedure, background) in [
                (
                    SETTINGS_CLASS,
                    Some(settings_proc as unsafe extern "system" fn(_, _, _, _) -> _),
                    (COLOR_BTNFACE + 1) as HBRUSH,
                ),
                (
                    HALO_CLASS,
                    Some(halo_proc as unsafe extern "system" fn(_, _, _, _) -> _),
                    null_mut(),
                ),
            ] {
                let class = WNDCLASSW {
                    lpfnWndProc: procedure,
                    hInstance: self.instance,
                    hIcon: icon,
                    hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                    hbrBackground: background,
                    lpszClassName: name,
                    ..zeroed()
                };
                if RegisterClassW(&class) == 0 {
                    return Err(os_error("Register window class"));
                }
            }
            self.dpi.set(GetDpiForSystem());
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let mut bounds = RECT {
                right: self.scale(450),
                bottom: self.scale(830),
                ..zeroed()
            };
            check(
                AdjustWindowRectExForDpi(
                    &mut bounds,
                    style,
                    0,
                    WS_EX_CONTROLPARENT,
                    self.dpi.get(),
                ),
                "Calculate settings window bounds",
            )?;
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                SETTINGS_CLASS,
                w!("Cursor Highlighter"),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                null_mut(),
                null_mut(),
                self.instance,
                null(),
            );
            if window.is_null() {
                return Err(os_error("Create settings window"));
            }
            self.window.set(window);
            self.dpi.set(GetDpiForWindow(window));
            self.add_controls()?;
            self.layout()?;
            let halo = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST,
                HALO_CLASS,
                w!("Cursor Highlighter Halo"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                self.instance,
                null(),
            );
            if halo.is_null() {
                return Err(os_error("Create halo overlay"));
            }
            self.halo.set(halo);
            let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), self.instance, 0);
            if hook.is_null() {
                return Err(os_error("Observe mouse clicks"));
            }
            self.hook.set(hook);
            let auto_hide = self.settings.borrow().auto_hide;
            self.set_auto_hide(auto_hide)?;
            let shortcut = self.settings.borrow().shortcut.clone();
            match self.register_shortcut(&shortcut) {
                Ok(()) => self.shortcut_status()?,
                Err(error) => self.text(
                    ID_STATUS,
                    &format!("{error}\r\nChoose another shortcut or use toggle mode."),
                )?,
            }
            self.update_labels()?;
            self.add_tray()?;
            let taskbar = RegisterWindowMessageW(w!("TaskbarCreated"));
            if taskbar == 0 {
                return Err(os_error("Register taskbar recovery message"));
            }
            self.taskbar_message.set(taskbar);
            if SetTimer(window, FRAME_TIMER, 16, None) == 0 {
                return Err(os_error("Start halo timer"));
            }
            self.tick()?;
            if visible {
                ShowWindow(window, SW_SHOWNORMAL);
                UpdateWindow(window);
            }
            Ok(())
        }
    }

    fn scale(&self, value: i32) -> i32 {
        (value * self.dpi.get() as i32 + 48) / 96
    }

    fn control(&self, id: u16) -> HWND {
        let parent = if (ID_UPDATE_AUTO..=ID_UPDATE_INSTALL).contains(&id) {
            self.updates_window.get()
        } else {
            self.window.get()
        };
        unsafe { GetDlgItem(parent, id as i32) }
    }

    fn text(&self, id: u16, text: &str) -> Result<(), String> {
        unsafe {
            check(
                SetWindowTextW(self.control(id), wide(text).as_ptr()),
                "Set control text",
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_control(
        &self,
        class: *const u16,
        text: &str,
        style: u32,
        id: u16,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Result<HWND, String> {
        let window = unsafe {
            CreateWindowExW(
                0,
                class,
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                self.scale(x),
                self.scale(y),
                self.scale(width),
                self.scale(height),
                self.window.get(),
                id as usize as HMENU,
                self.instance,
                null(),
            )
        };
        if window.is_null() {
            return Err(os_error("Create settings control"));
        }
        self.controls.borrow_mut().push(Control {
            window,
            x,
            y,
            width,
            height,
        });
        Ok(window)
    }

    fn add_controls(&self) -> Result<(), String> {
        self.add_control(
            w!("STATIC"),
            "Make your pointer stand out",
            0,
            0,
            24,
            20,
            400,
            24,
        )?;
        self.add_control(
            w!("STATIC"),
            "A translucent halo, without dimming your screen.",
            0,
            0,
            24,
            49,
            400,
            24,
        )?;
        let enabled = self.add_control(
            w!("BUTTON"),
            "Highlight cursor",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_ENABLED,
            24,
            85,
            390,
            25,
        )?;
        self.add_control(w!("STATIC"), "Presentation presets", 0, 0, 24, 120, 160, 24)?;
        self.add_control(
            w!("BUTTON"),
            "Slides",
            WS_TABSTOP,
            ID_PRESET_SLIDES,
            194,
            116,
            100,
            32,
        )?;
        self.add_control(
            w!("BUTTON"),
            "Live demo",
            WS_TABSTOP,
            ID_PRESET_DEMO,
            304,
            116,
            118,
            32,
        )?;
        self.add_control(w!("STATIC"), "", 0, ID_SIZE_LABEL, 24, 185, 390, 24)?;
        let diameter = self.add_control(
            TRACKBAR_CLASSW,
            "",
            TBS_AUTOTICKS | WS_TABSTOP,
            ID_DIAMETER,
            24,
            213,
            398,
            35,
        )?;
        let scale_with_dpi = self.add_control(
            w!("BUTTON"),
            "Scale size with monitor DPI",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_SCALE_WITH_DPI,
            24,
            254,
            398,
            25,
        )?;
        self.add_control(w!("STATIC"), "", 0, ID_OPACITY_LABEL, 24, 302, 390, 24)?;
        let opacity = self.add_control(
            TRACKBAR_CLASSW,
            "",
            TBS_AUTOTICKS | WS_TABSTOP,
            ID_OPACITY,
            24,
            330,
            398,
            35,
        )?;
        let outline = self.add_control(
            w!("BUTTON"),
            "Outline only (keep text readable)",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_OUTLINE,
            24,
            372,
            390,
            25,
        )?;
        self.add_control(w!("STATIC"), "Halo color", 0, 0, 24, 421, 90, 24)?;
        self.add_control(
            w!("BUTTON"),
            "",
            WS_TABSTOP | BS_OWNERDRAW as u32,
            ID_COLOR,
            124,
            413,
            190,
            33,
        )?;
        self.add_control(
            w!("BUTTON"),
            "Distinct clicks: blue left / orange right",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_CLICK_COLORS,
            24,
            459,
            400,
            25,
        )?;
        self.add_control(
            w!("BUTTON"),
            "Auto-hide when idle (1.5 s) or typing",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_AUTO_HIDE,
            24,
            490,
            400,
            25,
        )?;
        let start_minimized = self.add_control(
            w!("BUTTON"),
            "Start hidden in the tray",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_START_MINIMIZED,
            24,
            521,
            400,
            25,
        )?;
        let launch_at_login = self.add_control(
            w!("BUTTON"),
            "Launch at Windows login (current user)",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_LAUNCH_AT_LOGIN,
            24,
            552,
            400,
            25,
        )?;
        self.add_control(
            w!("STATIC"),
            "Shortcut (e.g. Ctrl + Alt + H)",
            0,
            0,
            24,
            590,
            400,
            24,
        )?;
        let shortcut = self.add_control(
            w!("EDIT"),
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            ID_SHORTCUT,
            24,
            618,
            270,
            28,
        )?;
        self.add_control(
            w!("BUTTON"),
            "Apply",
            WS_TABSTOP,
            ID_APPLY_SHORTCUT,
            308,
            618,
            90,
            28,
        )?;
        let hold = self.add_control(
            w!("BUTTON"),
            "Hold shortcut to highlight (instead of toggle)",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_HOLD,
            24,
            656,
            400,
            25,
        )?;
        self.add_control(w!("STATIC"), "", 0, ID_STATUS, 24, 694, 400, 80)?;
        self.add_control(
            w!("BUTTON"),
            "Hide to tray",
            WS_TABSTOP,
            ID_HIDE,
            24,
            785,
            115,
            32,
        )?;
        self.add_control(w!("BUTTON"), "Exit", WS_TABSTOP, ID_EXIT, 150, 785, 85, 32)?;
        self.add_control(
            w!("BUTTON"),
            "Updates...",
            WS_TABSTOP,
            ID_UPDATES,
            250,
            785,
            172,
            32,
        )?;
        let settings = self.settings.borrow().clone();
        unsafe {
            SendMessageW(shortcut, EM_SETLIMITTEXT, 80, 0);
            self.text(ID_SHORTCUT, &settings.shortcut.label())?;
            SendMessageW(
                hold,
                BM_SETCHECK,
                usize::from(settings.activation_mode == ActivationMode::Hold),
                0,
            );
            SendMessageW(enabled, BM_SETCHECK, usize::from(settings.enabled), 0);
            SendMessageW(outline, BM_SETCHECK, usize::from(settings.outline_only), 0);
            SendMessageW(
                start_minimized,
                BM_SETCHECK,
                usize::from(settings.start_minimized),
                0,
            );
            SendMessageW(
                launch_at_login,
                BM_SETCHECK,
                usize::from(self.launch_at_login.get()),
                0,
            );
            SendMessageW(
                scale_with_dpi,
                BM_SETCHECK,
                usize::from(settings.scale_with_dpi),
                0,
            );
            for (slider, min, max, value, frequency) in [
                (diameter, 24, 240, settings.diameter, 24),
                (opacity, 5, 80, settings.opacity, 5),
            ] {
                SendMessageW(slider, TBM_SETRANGE, 1, (min | (max << 16)) as isize);
                SendMessageW(slider, TBM_SETTICFREQ, frequency, 0);
                SendMessageW(slider, TBM_SETPOS, 1, value as isize);
            }
        }
        Ok(())
    }

    fn layout(&self) -> Result<(), String> {
        unsafe {
            let font = CreateFontW(
                -self.scale(14),
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                DEFAULT_PITCH as u32,
                w!("Segoe UI"),
            );
            if font.is_null() {
                return Err(os_error("Create settings font"));
            }
            let old = self.font.replace(font);
            let controls = self.controls.borrow().clone();
            for control in controls {
                SendMessageW(control.window, WM_SETFONT, font as usize, 1);
                check(
                    SetWindowPos(
                        control.window,
                        null_mut(),
                        self.scale(control.x),
                        self.scale(control.y),
                        self.scale(control.width),
                        self.scale(control.height),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    ),
                    "Scale settings control",
                )?;
            }
            if !old.is_null() {
                DeleteObject(old);
            }
            for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
                let size = GetSystemMetricsForDpi(metric, self.dpi.get());
                let icon = LoadImageW(
                    self.instance,
                    ICON_RESOURCE,
                    IMAGE_ICON,
                    size,
                    size,
                    LR_SHARED,
                );
                if icon.is_null() {
                    return Err(os_error("Load window icon"));
                }
                SendMessageW(self.window.get(), WM_SETICON, kind as usize, icon as isize);
            }
        }
        Ok(())
    }

    fn update_labels(&self) -> Result<(), String> {
        let settings = self.settings.borrow().clone();
        self.text(
            ID_SIZE_LABEL,
            &format!(
                "Size: {} {} diameter",
                settings.diameter,
                if settings.scale_with_dpi {
                    "logical px (96 DPI)"
                } else {
                    "physical px"
                }
            ),
        )?;
        self.text(ID_OPACITY_LABEL, &format!("Opacity: {}%", settings.opacity))?;
        self.text(ID_COLOR, &format!("{}   Choose...", settings.color_hex))?;
        unsafe {
            for (id, value) in [
                (ID_DIAMETER, settings.diameter),
                (ID_OPACITY, settings.opacity),
            ] {
                SendMessageW(self.control(id), TBM_SETPOS, 1, value as isize);
            }
            check(
                InvalidateRect(self.control(ID_COLOR), null(), 0),
                "Refresh color preview",
            )?;
            SendMessageW(
                self.control(ID_ENABLED),
                BM_SETCHECK,
                usize::from(settings.enabled),
                0,
            );
            SendMessageW(
                self.control(ID_CLICK_COLORS),
                BM_SETCHECK,
                usize::from(settings.distinct_click_colors),
                0,
            );
            SendMessageW(
                self.control(ID_SCALE_WITH_DPI),
                BM_SETCHECK,
                usize::from(settings.scale_with_dpi),
                0,
            );
            SendMessageW(
                self.control(ID_AUTO_HIDE),
                BM_SETCHECK,
                usize::from(settings.auto_hide),
                0,
            );
            SendMessageW(
                self.control(ID_OUTLINE),
                BM_SETCHECK,
                usize::from(settings.outline_only),
                0,
            );
        }
        Ok(())
    }

    fn draw_color_button(&self, item: &DRAWITEMSTRUCT) -> Result<(), String> {
        let settings = self.settings.borrow().clone();
        let [red, green, blue] = settings.rgb()?;
        let color = red as u32 | ((green as u32) << 8) | ((blue as u32) << 16);
        let text_color = if red.min(green).min(blue) as u32 + red.max(green).max(blue) as u32 >= 255
        {
            0
        } else {
            0xFFFFFF
        };
        unsafe {
            let saved = SaveDC(item.hDC);
            if saved == 0 {
                return Err(os_error("Save color button drawing state"));
            }
            let result = (|| {
                let brush = CreateSolidBrush(color);
                if brush.is_null() {
                    return Err(os_error("Create color preview brush"));
                }
                let fill = check(
                    FillRect(item.hDC, &item.rcItem, brush),
                    "Paint color preview",
                );
                DeleteObject(brush);
                fill?;
                let mut bounds = item.rcItem;
                let pressed = item.itemState & ODS_SELECTED != 0;
                check(
                    DrawEdge(
                        item.hDC,
                        &mut bounds,
                        if pressed { EDGE_SUNKEN } else { EDGE_RAISED },
                        BF_RECT,
                    ),
                    "Draw color button border",
                )?;
                if SelectObject(item.hDC, self.font.get()).is_null() {
                    return Err(os_error("Select color button font"));
                }
                if SetBkMode(item.hDC, TRANSPARENT as i32) == 0
                    || SetTextColor(item.hDC, text_color) == CLR_INVALID
                {
                    return Err(os_error("Set color button text appearance"));
                }
                if pressed {
                    OffsetRect(&mut bounds, self.scale(1), self.scale(1));
                }
                let text = wide(&format!("{}   Choose...", settings.color_hex));
                check(
                    DrawTextW(
                        item.hDC,
                        text.as_ptr(),
                        (text.len() - 1) as i32,
                        &mut bounds,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    ),
                    "Draw color button text",
                )?;
                if item.itemState & (ODS_FOCUS | ODS_NOFOCUSRECT) == ODS_FOCUS {
                    let mut focus = item.rcItem;
                    InflateRect(&mut focus, -self.scale(4), -self.scale(4));
                    check(
                        DrawFocusRect(item.hDC, &focus),
                        "Draw color button keyboard focus",
                    )?;
                }
                Ok(())
            })();
            RestoreDC(item.hDC, saved);
            result
        }
    }

    fn changed(&self) -> Result<(), String> {
        self.update_labels()?;
        self.appearance_dirty.set(true);
        self.dirty.set(true);
        unsafe {
            if SetTimer(self.window.get(), SAVE_TIMER, 400, None) == 0 {
                return Err(os_error("Schedule settings save"));
            }
        }
        self.tick()
    }

    fn save(&self) {
        unsafe {
            KillTimer(self.window.get(), SAVE_TIMER);
        }
        if !self.dirty.get() {
            return;
        }
        let settings = self.settings.borrow().clone();
        match settings.save(&self.settings_path) {
            Ok(()) => self.dirty.set(false),
            Err(error) => self.warning(&format!(
                "Your changes are active but could not be saved.\r\n\r\n{error}"
            )),
        }
    }

    fn warning(&self, message: &str) {
        unsafe {
            MessageBoxW(
                self.window.get(),
                wide(message).as_ptr(),
                w!("Cursor Highlighter"),
                MB_OK | MB_ICONWARNING,
            );
        }
    }

    fn fail(&self, error: String) {
        if self.fatal.borrow().is_some() {
            return;
        }
        *self.fatal.borrow_mut() = Some(error);
        unsafe {
            KillTimer(self.window.get(), FRAME_TIMER);
            PostQuitMessage(1);
        }
    }

    fn command(&self, id: u16) -> Result<(), String> {
        if self.updates.borrow().installing {
            return Ok(());
        }
        match id {
            ID_UPDATES => self.open_updates()?,
            ID_UPDATE_CHECK => {
                self.updates.borrow_mut().check(true);
                self.refresh_updates()?;
            }
            ID_UPDATE_AUTO => {
                self.settings.borrow_mut().automatic_updates =
                    unsafe { SendMessageW(self.control(ID_UPDATE_AUTO), BM_GETCHECK, 0, 0) != 0 };
                self.updates
                    .borrow_mut()
                    .set_automatic(self.settings.borrow().automatic_updates);
                self.changed()?;
                self.refresh_updates()?;
            }
            ID_UPDATE_INSTALL => self.install_update()?,
            ID_APPLY_SHORTCUT => self.apply_shortcut()?,
            ID_HOLD => {
                let hold = unsafe { SendMessageW(self.control(ID_HOLD), BM_GETCHECK, 0, 0) != 0 };
                self.settings.borrow_mut().activation_mode = if hold {
                    ActivationMode::Hold
                } else {
                    ActivationMode::Toggle
                };
                self.hold_active.set(false);
                self.shortcut_status()?;
                self.changed()?;
            }
            ID_ENABLED => {
                let enabled =
                    unsafe { SendMessageW(self.control(ID_ENABLED), BM_GETCHECK, 0, 0) != 0 };
                self.settings.borrow_mut().enabled = enabled;
                self.auto_hide.replace(AutoHide::new(self.now()));
                self.changed()?;
            }
            ID_AUTO_HIDE => {
                let enabled =
                    unsafe { SendMessageW(self.control(ID_AUTO_HIDE), BM_GETCHECK, 0, 0) != 0 };
                self.set_auto_hide(enabled)?;
                self.settings.borrow_mut().auto_hide = enabled;
                self.changed()?;
            }
            ID_OUTLINE => {
                let outline_only =
                    unsafe { SendMessageW(self.control(ID_OUTLINE), BM_GETCHECK, 0, 0) != 0 };
                self.settings.borrow_mut().outline_only = outline_only;
                self.changed()?;
            }
            ID_COLOR => self.choose_color()?,
            ID_CLICK_COLORS => {
                let enabled =
                    unsafe { SendMessageW(self.control(ID_CLICK_COLORS), BM_GETCHECK, 0, 0) != 0 };
                self.settings.borrow_mut().distinct_click_colors = enabled;
                self.changed()?;
            }
            ID_START_MINIMIZED => {
                self.settings.borrow_mut().start_minimized = unsafe {
                    SendMessageW(self.control(ID_START_MINIMIZED), BM_GETCHECK, 0, 0)
                        == BST_CHECKED as isize
                };
                self.changed()?;
            }
            ID_LAUNCH_AT_LOGIN => {
                let requested = unsafe {
                    SendMessageW(self.control(ID_LAUNCH_AT_LOGIN), BM_GETCHECK, 0, 0)
                        == BST_CHECKED as isize
                };
                match startup::set_enabled(requested) {
                    Ok(()) => self.launch_at_login.set(requested),
                    Err(error) => {
                        unsafe {
                            SendMessageW(
                                self.control(ID_LAUNCH_AT_LOGIN),
                                BM_SETCHECK,
                                usize::from(self.launch_at_login.get()),
                                0,
                            );
                        }
                        self.warning(&format!(
                            "Could not change launch at Windows login.\r\n\r\n{error}"
                        ));
                    }
                }
            }
            ID_SCALE_WITH_DPI => {
                let scale_with_dpi = unsafe {
                    SendMessageW(self.control(ID_SCALE_WITH_DPI), BM_GETCHECK, 0, 0) != 0
                };
                self.settings.borrow_mut().scale_with_dpi = scale_with_dpi;
                self.changed()?;
            }
            ID_PRESET_SLIDES | ID_PRESET_DEMO => {
                let preset = if id == ID_PRESET_SLIDES {
                    PresentationPreset::Slides
                } else {
                    PresentationPreset::LiveDemo
                };
                self.settings.borrow_mut().apply_preset(preset);
                self.changed()?;
            }
            ID_HIDE => {
                self.save();
                unsafe {
                    ShowWindow(self.window.get(), SW_HIDE);
                }
            }
            ID_EXIT => unsafe {
                SendMessageW(self.window.get(), WM_CLOSE, 0, 0);
            },
            ID_OPEN => self.open(),
            _ => {}
        }
        Ok(())
    }

    fn toggle(&self) -> Result<(), String> {
        if self.updates.borrow().installing {
            return Ok(());
        }
        {
            let mut settings = self.settings.borrow_mut();
            settings.enabled = !settings.enabled;
        }
        self.auto_hide.replace(AutoHide::new(self.now()));
        self.changed()
    }

    fn register_shortcut(&self, shortcut: &Shortcut) -> Result<(), String> {
        shortcut.validate()?;
        let old_id = self.hotkey_id.get();
        if old_id != 0 && *shortcut == self.settings.borrow().shortcut {
            return Ok(());
        }
        let id = if old_id == 1 { 2 } else { 1 };
        let modifiers = MOD_NOREPEAT
            | if shortcut.control { MOD_CONTROL } else { 0 }
            | if shortcut.alt { MOD_ALT } else { 0 }
            | if shortcut.shift { MOD_SHIFT } else { 0 }
            | if shortcut.win { MOD_WIN } else { 0 };
        unsafe {
            if RegisterHotKey(self.window.get(), id, modifiers, shortcut.key) == 0 {
                let error = GetLastError();
                return Err(format!(
                    "{} unavailable (Windows error {error}).\r\nAlready in use or reserved.",
                    shortcut.label(),
                ));
            }
            if old_id != 0 && UnregisterHotKey(self.window.get(), old_id) == 0 {
                let error = os_error("Release previous shortcut");
                if UnregisterHotKey(self.window.get(), id) == 0 {
                    return Err(format!(
                        "{error}; {}",
                        os_error("Release replacement shortcut")
                    ));
                }
                return Err(error);
            }
        }
        self.hotkey_id.set(id);
        self.hold_active.set(false);
        Ok(())
    }

    fn shortcut_status(&self) -> Result<(), String> {
        let settings = self.settings.borrow().clone();
        let action = if settings.activation_mode == ActivationMode::Hold {
            "Hold to highlight; release any chord key to hide.\r\nHighlight cursor must be enabled."
        } else {
            "Press to toggle the halo."
        };
        let status = if self.hotkey_id.get() == 0 {
            "Shortcut unavailable. Choose another shortcut or use toggle mode.".to_string()
        } else {
            format!("{}\r\n{action}", settings.shortcut.label())
        };
        self.text(ID_STATUS, &status)
    }

    fn apply_shortcut(&self) -> Result<(), String> {
        let mut text = [0u16; 81];
        let length = unsafe {
            GetWindowTextW(
                self.control(ID_SHORTCUT),
                text.as_mut_ptr(),
                text.len() as i32,
            )
        };
        let candidate = Shortcut::parse(&String::from_utf16_lossy(&text[..length as usize]));
        let result = candidate.and_then(|shortcut| {
            self.register_shortcut(&shortcut)?;
            self.settings.borrow_mut().shortcut = shortcut;
            Ok(())
        });
        if let Err(error) = result {
            self.text(
                ID_STATUS,
                &format!("{error}\r\nPrevious shortcut/settings unchanged."),
            )?;
            return Ok(());
        }
        self.text(ID_SHORTCUT, &self.settings.borrow().shortcut.label())?;
        self.shortcut_status()?;
        self.changed()
    }

    fn shortcut_pressed(&self) -> Result<(), String> {
        if self.settings.borrow().activation_mode == ActivationMode::Toggle {
            self.toggle()
        } else {
            self.hold_active.set(true);
            self.tick()
        }
    }

    fn set_auto_hide(&self, enabled: bool) -> Result<(), String> {
        unsafe {
            if enabled && self.keyboard_hook.get().is_null() {
                let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), self.instance, 0);
                if hook.is_null() {
                    return Err(os_error("Observe keyboard activity for auto-hide"));
                }
                self.keyboard_hook.set(hook);
            } else if !enabled && !self.keyboard_hook.get().is_null() {
                check(
                    UnhookWindowsHookEx(self.keyboard_hook.get()),
                    "Remove keyboard activity observer",
                )?;
                self.keyboard_hook.set(null_mut());
            }
        }
        self.auto_hide.replace(AutoHide::new(self.now()));
        Ok(())
    }

    fn observe_movement(&self, x: i32, y: i32, now: u64) {
        if self.cursor_position.replace(Some((x, y))) != Some((x, y)) {
            self.auto_hide.borrow_mut().moved(now);
        }
    }

    fn choose_color(&self) -> Result<(), String> {
        let rgb = self.settings.borrow().rgb()?;
        let mut custom = [0u32; 16];
        let mut dialog = CHOOSECOLORW {
            lStructSize: size_of::<CHOOSECOLORW>() as u32,
            hwndOwner: self.window.get(),
            rgbResult: rgb[0] as u32 | ((rgb[1] as u32) << 8) | ((rgb[2] as u32) << 16),
            lpCustColors: custom.as_mut_ptr(),
            Flags: CC_FULLOPEN | CC_RGBINIT,
            ..unsafe { zeroed() }
        };
        // The native modal loop continues delivering our halo timer and hook.
        if unsafe { ChooseColorW(&mut dialog) } != 0 {
            self.settings.borrow_mut().color_hex = format!(
                "#{:02X}{:02X}{:02X}",
                dialog.rgbResult & 255,
                (dialog.rgbResult >> 8) & 255,
                (dialog.rgbResult >> 16) & 255
            );
            self.changed()?;
        } else {
            let error = unsafe { CommDlgExtendedError() };
            if error != 0 {
                return Err(format!("Open color picker failed: 0x{error:08X}"));
            }
        }
        Ok(())
    }

    fn open(&self) {
        unsafe {
            ShowWindow(self.window.get(), SW_RESTORE);
            SetForegroundWindow(self.window.get());
        }
    }

    fn open_updates(&self) -> Result<(), String> {
        self.updates_dialog(true)
    }

    fn updates_dialog(&self, visible: bool) -> Result<(), String> {
        unsafe {
            if self.updates_window.get().is_null() {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(updates_proc),
                    hInstance: self.instance,
                    hIcon: LoadIconW(self.instance, ICON_RESOURCE),
                    hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                    hbrBackground: (COLOR_BTNFACE + 1) as HBRUSH,
                    lpszClassName: UPDATES_CLASS,
                    ..zeroed()
                };
                if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                    return Err(os_error("Register updates window"));
                }
                let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
                let mut bounds = RECT {
                    right: self.scale(450),
                    bottom: self.scale(290),
                    ..zeroed()
                };
                check(
                    AdjustWindowRectExForDpi(
                        &mut bounds,
                        style,
                        0,
                        WS_EX_CONTROLPARENT,
                        self.dpi.get(),
                    ),
                    "Size updates window",
                )?;
                let window = CreateWindowExW(
                    WS_EX_CONTROLPARENT,
                    UPDATES_CLASS,
                    w!("Cursor Highlighter updates"),
                    style,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    bounds.right - bounds.left,
                    bounds.bottom - bounds.top,
                    self.window.get(),
                    null_mut(),
                    self.instance,
                    null(),
                );
                if window.is_null() {
                    return Err(os_error("Create updates window"));
                }
                self.updates_window.set(window);
                self.update_dialog_text.borrow_mut().clear();
                let result = (|| {
                    for (id, class, label, style, x, y, width, height) in [
                        (
                            ID_UPDATE_AUTO,
                            w!("BUTTON"),
                            "Automatically check and download updates",
                            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
                            20,
                            20,
                            410,
                            28,
                        ),
                        (
                            ID_UPDATE_STATUS,
                            w!("EDIT"),
                            "",
                            WS_VSCROLL | WS_TABSTOP | ES_MULTILINE as u32 | ES_READONLY as u32,
                            20,
                            60,
                            410,
                            110,
                        ),
                        (
                            0,
                            w!("STATIC"),
                            "Installing always asks first. No automatic restarts.",
                            0,
                            20,
                            174,
                            410,
                            30,
                        ),
                        (
                            ID_UPDATE_CHECK,
                            w!("BUTTON"),
                            "Check for updates",
                            WS_TABSTOP,
                            20,
                            225,
                            180,
                            34,
                        ),
                        (
                            ID_UPDATE_INSTALL,
                            w!("BUTTON"),
                            "Install and restart...",
                            WS_TABSTOP,
                            215,
                            225,
                            215,
                            34,
                        ),
                    ] {
                        let control = CreateWindowExW(
                            0,
                            class,
                            wide(label).as_ptr(),
                            WS_CHILD | WS_VISIBLE | style,
                            self.scale(x),
                            self.scale(y),
                            self.scale(width),
                            self.scale(height),
                            window,
                            id as usize as HMENU,
                            self.instance,
                            null(),
                        );
                        if control.is_null() {
                            return Err(os_error("Create updates control"));
                        }
                        self.updates_controls.borrow_mut().push(Control {
                            window: control,
                            x,
                            y,
                            width,
                            height,
                        });
                    }
                    let dpi = GetDpiForWindow(window);
                    self.layout_updates(dpi)?;
                    let mut bounds = RECT {
                        right: (450 * dpi as i32 + 48) / 96,
                        bottom: (290 * dpi as i32 + 48) / 96,
                        ..zeroed()
                    };
                    check(
                        AdjustWindowRectExForDpi(&mut bounds, style, 0, WS_EX_CONTROLPARENT, dpi),
                        "Size updates window on its initial monitor",
                    )?;
                    check(
                        SetWindowPos(
                            window,
                            null_mut(),
                            0,
                            0,
                            bounds.right - bounds.left,
                            bounds.bottom - bounds.top,
                            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                        ),
                        "Apply updates window size",
                    )?;
                    Ok(())
                })();
                if let Err(error) = result {
                    DestroyWindow(window);
                    self.updates_window.set(null_mut());
                    self.updates_controls.borrow_mut().clear();
                    return Err(error);
                }
            }
            self.refresh_updates()?;
            if visible {
                ShowWindow(self.updates_window.get(), SW_SHOWNORMAL);
                SetForegroundWindow(self.updates_window.get());
            }
        }
        Ok(())
    }

    fn layout_updates(&self, dpi: u32) -> Result<(), String> {
        let scale = |value: i32| (value * dpi as i32 + 48) / 96;
        unsafe {
            let font = CreateFontW(
                -scale(14),
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                DEFAULT_PITCH as u32,
                w!("Segoe UI"),
            );
            if font.is_null() {
                return Err(os_error("Create updates font"));
            }
            let old = self.updates_font.replace(font);
            for control in self.updates_controls.borrow().iter() {
                SendMessageW(control.window, WM_SETFONT, font as usize, 1);
                check(
                    SetWindowPos(
                        control.window,
                        null_mut(),
                        scale(control.x),
                        scale(control.y),
                        scale(control.width),
                        scale(control.height),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    ),
                    "Scale updates control",
                )?;
            }
            if !old.is_null() {
                DeleteObject(old);
            }
        }
        Ok(())
    }

    fn refresh_updates(&self) -> Result<(), String> {
        let (status, ready, busy, installing) = {
            let updates = self.updates.borrow();
            (
                updates.status.clone(),
                updates.staged.is_some(),
                updates.busy(),
                updates.installing,
            )
        };
        unsafe {
            EnableWindow(self.window.get(), i32::from(!installing));
        }
        if !self.updates_window.get().is_null() {
            let automatic = self.settings.borrow().automatic_updates;
            let text = format!(
                "{status}\r\n\r\nAutomatic checks/downloads: {}.",
                if automatic {
                    "on"
                } else {
                    "off (manual checks still available)"
                }
            );
            if *self.update_dialog_text.borrow() != text {
                self.text(ID_UPDATE_STATUS, &text)?;
                *self.update_dialog_text.borrow_mut() = text;
            }
            unsafe {
                SendMessageW(
                    self.control(ID_UPDATE_AUTO),
                    BM_SETCHECK,
                    usize::from(automatic),
                    0,
                );
                EnableWindow(
                    self.control(ID_UPDATE_CHECK),
                    i32::from(!busy && !installing),
                );
                EnableWindow(
                    self.control(ID_UPDATE_INSTALL),
                    i32::from(ready && !busy && !installing),
                );
                EnableWindow(self.control(ID_UPDATE_AUTO), i32::from(!installing));
            }
        }
        let summary = if ready {
            "Update ready..."
        } else {
            "Updates..."
        };
        self.text(ID_UPDATES, summary)?;
        let summary = format!("{summary} {status}");
        if *self.update_summary.borrow() != summary && self.tray_added.get() {
            let mut data = self.tray_data();
            let label = if ready {
                "Cursor Highlighter - update ready; open Updates to install".to_string()
            } else if status.starts_with("Update failed:") || status.starts_with("Cannot install") {
                "Cursor Highlighter - update error; see Updates".to_string()
            } else {
                "Cursor Highlighter".to_string()
            };
            data.szTip.fill(0);
            for (to, from) in data.szTip.iter_mut().zip(wide(&label)) {
                *to = from;
            }
            if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } == 0 {
                eprintln!(
                    "{}",
                    os_error(
                        "Update notification icon status; status remains available in Updates"
                    )
                );
            }
            *self.update_summary.borrow_mut() = summary;
        }
        Ok(())
    }

    fn poll_updates(&self) -> Result<(), String> {
        let automatic = self.settings.borrow().automatic_updates;
        let handoff = self.updates.borrow_mut().poll(automatic);
        if handoff.is_some() {
            // Settings were persisted before the helper was launched. Exit the
            // message loop without a second, potentially failing settings write.
            unsafe {
                PostQuitMessage(0);
            }
            return Ok(());
        }
        self.refresh_updates()
    }

    fn install_update(&self) -> Result<(), String> {
        let version = {
            let updates = self.updates.borrow();
            if updates.busy() || updates.installing {
                return Ok(());
            }
            updates
                .staged
                .as_ref()
                .map(|stage| stage.version.to_string())
        };
        let Some(version) = version else {
            return Ok(());
        };
        let confirmed = unsafe {
            MessageBoxW(
                    self.updates_window.get(),
                    wide(&format!("Install Cursor Highlighter v{version} and restart now?\r\n\r\nThe halo will stop briefly. Choose No to keep presenting; the update stays downloaded.")).as_ptr(),
                    w!("Install update?"), MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
                ) == IDYES
        };
        if !confirmed {
            return Ok(());
        }
        if let Err(error) = self.settings.borrow().save(&self.settings_path) {
            self.updates.borrow_mut().status =
                format!("Cannot install until settings are saved: {error}");
        } else {
            self.dirty.set(false);
            unsafe {
                KillTimer(self.window.get(), SAVE_TIMER);
            }
            self.updates.borrow_mut().install();
        }
        self.refresh_updates()
    }
    fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = unsafe { zeroed() };
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.window.get();
        data.uID = 1;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = TRAY_MESSAGE;
        data.hIcon = unsafe { LoadIconW(self.instance, ICON_RESOURCE) };
        for (destination, source) in data.szTip.iter_mut().zip(wide("Cursor Highlighter")) {
            *destination = source;
        }
        data
    }

    fn add_tray(&self) -> Result<(), String> {
        check(
            unsafe { Shell_NotifyIconW(NIM_ADD, &self.tray_data()) },
            "Add notification-area icon",
        )?;
        self.tray_added.set(true);
        self.update_summary.borrow_mut().clear();
        Ok(())
    }

    fn tray_menu(&self) -> Result<(), String> {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return Err(os_error("Create tray menu"));
            }
            let result = (|| {
                check(
                    AppendMenuW(menu, MF_STRING, ID_OPEN as usize, w!("Settings")),
                    "Add tray settings item",
                )?;
                let flags = MF_STRING
                    | if self.settings.borrow().enabled {
                        MF_CHECKED
                    } else {
                        MF_UNCHECKED
                    };
                // This item toggles state, unlike the settings checkbox notification.
                check(
                    AppendMenuW(menu, flags, 200, w!("Highlight cursor")),
                    "Add tray toggle item",
                )?;
                check(
                    AppendMenuW(menu, MF_SEPARATOR, 0, null()),
                    "Add tray separator",
                )?;
                for (id, label) in [
                    (ID_PRESET_SLIDES, w!("Slides preset")),
                    (ID_PRESET_DEMO, w!("Live demo preset")),
                    (ID_UPDATES, w!("Updates...")),
                ] {
                    check(
                        AppendMenuW(menu, MF_STRING, id as usize, label),
                        "Add tray presentation preset",
                    )?;
                }
                check(
                    AppendMenuW(menu, MF_SEPARATOR, 0, null()),
                    "Add tray preset separator",
                )?;
                check(
                    AppendMenuW(menu, MF_STRING, ID_EXIT as usize, w!("Exit")),
                    "Add tray exit item",
                )?;
                let mut point = zeroed();
                check(GetCursorPos(&mut point), "Get tray menu position")?;
                SetForegroundWindow(self.window.get());
                let choice = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_RIGHTBUTTON,
                    point.x,
                    point.y,
                    0,
                    self.window.get(),
                    null(),
                ) as u16;
                PostMessageW(self.window.get(), WM_NULL, 0, 0);
                if choice == 200 {
                    self.toggle()
                } else {
                    self.command(choice)
                }
            })();
            DestroyMenu(menu);
            result
        }
    }

    fn tick(&self) -> Result<(), String> {
        self.tick_at(self.now())
    }

    fn tick_at(&self, now: u64) -> Result<(), String> {
        let settings = self.settings.borrow().clone();
        // Inspect only this chord's keys. No keyboard hook or key history.
        if self.hold_active.get()
            && (!settings.enabled
                || !chord_held(&settings.shortcut, |key| unsafe {
                    GetAsyncKeyState(key) < 0
                }))
        {
            self.hold_active.set(false);
        }
        if !settings
            .activation_mode
            .visible(settings.enabled, self.hold_active.get())
        {
            self.click.borrow_mut().reset();
            unsafe {
                ShowWindow(self.halo.get(), SW_HIDE);
            }
            self.appearance_dirty.set(true);
            return Ok(());
        }
        let mut cursor: POINT = unsafe { zeroed() };
        check(
            unsafe { GetCursorPos(&mut cursor) },
            "Get desktop cursor position",
        )?;
        self.observe_movement(cursor.x, cursor.y, now);
        let alpha = halo_alpha(&settings, &mut self.auto_hide.borrow_mut(), now);
        if alpha == 0 {
            unsafe {
                ShowWindow(self.halo.get(), SW_HIDE);
            }
            return Ok(());
        }
        let dpi = if settings.scale_with_dpi {
            self.cursor_dpi(cursor)?
        } else {
            96
        };
        let diameter = settings.pixel_diameter(dpi)?;
        let amount = self.click.borrow().amount(now);
        let colors = if settings.distinct_click_colors {
            self.click.borrow().color_amounts(now)
        } else {
            [0.0; 2]
        };
        if self.appearance_dirty.get()
            || self.rendered.get() != amount
            || self.rendered_alpha.get() != alpha
            || self.rendered_diameter.get() != diameter
            || self.rendered_colors.get() != colors
        {
            let pixels = render(&settings, amount, dpi, colors)?;
            self.upload(&pixels, diameter as i32 + 4, cursor, alpha)?;
            self.rendered.set(amount);
            self.rendered_alpha.set(alpha);
            self.rendered_diameter.set(diameter);
            self.rendered_colors.set(colors);
            self.appearance_dirty.set(false);
        }
        unsafe {
            let size = diameter as i32 + 4;
            let position = halo_position(cursor, size);
            check(
                SetWindowPos(
                    self.halo.get(),
                    HWND_TOPMOST,
                    position.x,
                    position.y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                ),
                "Position halo",
            )?;
        }
        Ok(())
    }

    fn cursor_dpi(&self, cursor: POINT) -> Result<u32, String> {
        unsafe {
            // A hidden 1px window selects the cursor's monitor even when the halo
            // straddles a boundary. GetDpiForWindow respects Per-Monitor V2.
            if self.dpi_probe.get().is_null() {
                let probe = CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    HALO_CLASS,
                    w!("Cursor Highlighter DPI probe"),
                    WS_POPUP,
                    cursor.x,
                    cursor.y,
                    1,
                    1,
                    null_mut(),
                    null_mut(),
                    self.instance,
                    null(),
                );
                if probe.is_null() {
                    return Err(os_error("Create hidden monitor DPI window"));
                }
                self.dpi_probe.set(probe);
            } else if MonitorFromWindow(self.dpi_probe.get(), MONITOR_DEFAULTTONEAREST)
                != MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
            {
                check(
                    SetWindowPos(
                        self.dpi_probe.get(),
                        null_mut(),
                        cursor.x,
                        cursor.y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    ),
                    "Move hidden monitor DPI window",
                )?;
            }
            let dpi = GetDpiForWindow(self.dpi_probe.get());
            if dpi == 0 {
                return Err(os_error("Get cursor monitor DPI"));
            }
            Ok(dpi)
        }
    }

    fn upload(&self, pixels: &[u8], size: i32, cursor: POINT, alpha: u8) -> Result<(), String> {
        unsafe {
            let screen = GetDC(null_mut());
            if screen.is_null() {
                return Err(os_error("Get screen DC"));
            }
            let memory = CreateCompatibleDC(screen);
            if memory.is_null() {
                ReleaseDC(null_mut(), screen);
                return Err(os_error("Create halo DC"));
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size,
                    biHeight: -size,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..zeroed()
                },
                ..zeroed()
            };
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(memory, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() {
                DeleteDC(memory);
                ReleaseDC(null_mut(), screen);
                return Err(os_error("Create alpha bitmap"));
            }
            let previous = SelectObject(memory, bitmap);
            let result = if previous.is_null() || previous == GDI_ERROR as HGDIOBJ {
                Err(os_error("Select halo bitmap"))
            } else {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
                let result = {
                    let position = halo_position(cursor, size);
                    let dimensions = SIZE { cx: size, cy: size };
                    let blend = BLENDFUNCTION {
                        BlendOp: AC_SRC_OVER as u8,
                        SourceConstantAlpha: alpha,
                        AlphaFormat: AC_SRC_ALPHA as u8,
                        BlendFlags: 0,
                    };
                    check(
                        UpdateLayeredWindow(
                            self.halo.get(),
                            screen,
                            &position,
                            &dimensions,
                            memory,
                            &POINT { x: 0, y: 0 },
                            0,
                            &blend,
                            ULW_ALPHA,
                        ),
                        "Upload translucent halo",
                    )
                };
                SelectObject(memory, previous);
                result
            };
            DeleteObject(bitmap);
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
            result
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            if !self.window.get().is_null() {
                KillTimer(self.window.get(), FRAME_TIMER);
                KillTimer(self.window.get(), SAVE_TIMER);
                KillTimer(self.window.get(), UPDATE_TIMER);
            }
            if !self.hook.get().is_null() && UnhookWindowsHookEx(self.hook.get()) == 0 {
                eprintln!("{}", os_error("Remove mouse hook"));
            }
            if !self.keyboard_hook.get().is_null()
                && UnhookWindowsHookEx(self.keyboard_hook.get()) == 0
            {
                eprintln!("{}", os_error("Remove keyboard hook"));
            }
            if self.hotkey_id.get() != 0 {
                UnregisterHotKey(self.window.get(), self.hotkey_id.get());
            }
            if self.tray_added.get() {
                Shell_NotifyIconW(NIM_DELETE, &self.tray_data());
            }
            if !self.halo.get().is_null() {
                DestroyWindow(self.halo.get());
            }
            if !self.dpi_probe.get().is_null() {
                DestroyWindow(self.dpi_probe.get());
            }
            if IsWindow(self.window.get()) != 0 {
                DestroyWindow(self.window.get());
            }
            if !self.updates_font.get().is_null() {
                DeleteObject(self.updates_font.get());
            }
            if !self.font.get().is_null() {
                DeleteObject(self.font.get());
            }
            UnregisterClassW(HALO_CLASS, self.instance);
            UnregisterClassW(SETTINGS_CLASS, self.instance);
            UnregisterClassW(UPDATES_CLASS, self.instance);
        }
    }
}

unsafe extern "system" fn mouse_proc(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code >= 0 {
        if message as u32 == WM_MOUSEMOVE {
            let event = unsafe { &*(data as *const MSLLHOOKSTRUCT) };
            with_app(|app| app.observe_movement(event.pt.x, event.pt.y, app.now()));
        }
        let action = match message as u32 {
            WM_LBUTTONDOWN => Some((1, true)),
            WM_LBUTTONUP => Some((1, false)),
            WM_RBUTTONDOWN => Some((2, true)),
            WM_RBUTTONUP => Some((2, false)),
            WM_MBUTTONDOWN => Some((4, true)),
            WM_MBUTTONUP => Some((4, false)),
            _ => None,
        };
        if let Some((button, down)) = action {
            with_app(|app| {
                if app.settings.borrow().enabled {
                    app.click.borrow_mut().button(button, down, app.now());
                }
            });
        }
    }
    // This observer never suppresses input or performs rendering in the hook.
    unsafe { CallNextHookEx(null_mut(), code, message, data) }
}

unsafe extern "system" fn keyboard_proc(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code >= 0 && matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        with_app(|app| {
            let settings = app.settings.borrow();
            if settings.enabled && settings.auto_hide {
                // Key identities and contents are neither read nor retained.
                app.auto_hide.borrow_mut().typed(app.now());
            }
        });
    }
    unsafe { CallNextHookEx(null_mut(), code, message, data) }
}

unsafe extern "system" fn halo_proc(window: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_NCHITTEST => HTTRANSPARENT as isize,
        // The frame timer uses the cursor's monitor, not the overlay's majority area.
        WM_DPICHANGED => 0,
        _ => unsafe { DefWindowProcW(window, message, wp, lp) },
    }
}

unsafe extern "system" fn settings_proc(
    window: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    let handled = with_app(|app| {
        if app.window.get() != window {
            return None;
        }
        let result = match message {
            WM_DRAWITEM if wp == ID_COLOR as usize => {
                let item = unsafe { &*(lp as *const DRAWITEMSTRUCT) };
                if let Err(error) = app.draw_color_button(item) {
                    app.fail(error);
                }
                return Some(1);
            }
            WM_COMMAND if (wp >> 16) & 0xFFFF == BN_CLICKED as usize => {
                app.command((wp & 0xFFFF) as u16)
            }
            WM_HSCROLL => {
                if app.updates.borrow().installing {
                    return Some(0);
                }
                let slider = lp as HWND;
                let value = unsafe { SendMessageW(slider, TBM_GETPOS, 0, 0) } as u32;
                if slider == app.control(ID_DIAMETER) {
                    app.settings.borrow_mut().diameter = value;
                } else if slider == app.control(ID_OPACITY) {
                    app.settings.borrow_mut().opacity = value;
                } else {
                    return None;
                }
                app.changed()
            }
            WM_TIMER if wp == FRAME_TIMER => app.tick(),
            WM_TIMER if wp == SAVE_TIMER => {
                app.save();
                Ok(())
            }
            WM_TIMER if wp == UPDATE_TIMER => {
                if let Err(error) = app.poll_updates() {
                    eprintln!("Update interface error: {error}");
                    app.updates.borrow_mut().status = format!("Update interface error: {error}");
                }
                Ok(())
            }
            WM_HOTKEY if app.hotkey_id.get() != 0 && wp == app.hotkey_id.get() as usize => {
                if app.updates.borrow().installing {
                    Ok(())
                } else {
                    app.shortcut_pressed()
                }
            }
            WM_CLOSE | WM_DESTROY => {
                app.save();
                unsafe {
                    ShowWindow(app.halo.get(), SW_HIDE);
                    PostQuitMessage(0);
                }
                Ok(())
            }
            WM_DPICHANGED => {
                app.dpi.set((wp & 0xFFFF) as u32);
                let rect = unsafe { &*(lp as *const RECT) };
                unsafe {
                    SetWindowPos(
                        window,
                        null_mut(),
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                app.layout()
            }
            TRAY_MESSAGE => match lp as u32 {
                WM_LBUTTONDBLCLK => {
                    app.open();
                    Ok(())
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => app.tray_menu(),
                _ => Ok(()),
            },
            _ if message == app.taskbar_message.get() && message != 0 => app.add_tray(),
            _ => return None,
        };
        if let Err(error) = result {
            app.fail(error);
        }
        Some(0)
    })
    .flatten();
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(window, message, wp, lp) })
}

unsafe extern "system" fn updates_proc(
    window: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    let handled = with_app(|app| {
        if app.updates_window.get() != window {
            return None;
        }
        let result = match message {
            WM_CLOSE => {
                unsafe {
                    ShowWindow(window, SW_HIDE);
                }
                Ok(())
            }
            WM_COMMAND if (wp >> 16) & 0xFFFF == BN_CLICKED as usize => {
                app.command((wp & 0xFFFF) as u16)
            }
            WM_DPICHANGED => {
                let rect = unsafe { &*(lp as *const RECT) };
                unsafe {
                    SetWindowPos(
                        window,
                        null_mut(),
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                app.layout_updates((wp & 0xFFFF) as u32)
            }
            _ => return None,
        };
        if let Err(error) = result {
            app.warning(&error);
        }
        Some(0)
    })
    .flatten();
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(window, message, wp, lp) })
}

struct Instance(HANDLE);
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct TestWindow(HWND);
impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

fn settings_path() -> Result<PathBuf, String> {
    let root = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not available.")?;
    Ok(PathBuf::from(root)
        .join("CursorHighlighter")
        .join("settings.json"))
}

pub fn run() -> Result<(), String> {
    let mutex = unsafe { CreateMutexW(null(), 0, w!("Local\\CursorHighlighter.Desktop")) };
    if mutex.is_null() {
        return Err(os_error("Create single-instance guard"));
    }
    let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let _instance = Instance(mutex);
    if existing {
        unsafe {
            MessageBoxW(
                null_mut(),
                w!(
                    "Cursor Highlighter is already running. Open Settings from its notification-area icon."
                ),
                w!("Cursor Highlighter"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        return Ok(());
    }
    let path = settings_path()?;
    let (settings, warning) = match Settings::load(&path) {
        Ok(settings) => (settings, None),
        Err(error) => (
            Settings::default(),
            Some(format!(
                "Could not load saved settings. Defaults are in use.\r\n\r\n{error}"
            )),
        ),
    };
    let visible = !settings.start_minimized;
    let login = startup::enabled();
    let app = Box::new(App::new(settings, path));
    if let Ok(enabled) = &login {
        app.launch_at_login.set(*enabled);
    }
    CURRENT.with(|current| current.set(&*app));
    let result = (|| {
        app.setup(visible)?;
        updater_start(&app)?;
        if let Err(error) = login {
            unsafe {
                SendMessageW(
                    app.control(ID_LAUNCH_AT_LOGIN),
                    BM_SETCHECK,
                    BST_INDETERMINATE as usize,
                    0,
                );
                EnableWindow(app.control(ID_LAUNCH_AT_LOGIN), 0);
            }
            app.warning(&format!(
                "Could not read launch-at-login settings. That control is unavailable until you restart the app.\r\n\r\n{error}"
            ));
        }
        if let Some(message) = warning {
            app.warning(&message);
        }
        unsafe {
            let mut message: MSG = zeroed();
            loop {
                let status = GetMessageW(&mut message, null_mut(), 0, 0);
                if status == -1 {
                    return Err(os_error("Read window message"));
                }
                if status == 0 {
                    break;
                }
                if (app.updates_window.get().is_null()
                    || IsDialogMessageW(app.updates_window.get(), &message) == 0)
                    && IsDialogMessageW(app.window.get(), &message) == 0
                {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        match app.fatal.borrow_mut().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    })();
    CURRENT.with(|current| current.set(null()));
    drop(app);
    result
}

fn updater_start(app: &App) -> Result<(), String> {
    crate::updater::finish_restart()?;
    app.updates
        .borrow_mut()
        .set_automatic(app.settings.borrow().automatic_updates);
    check(
        unsafe { SetTimer(app.window.get(), UPDATE_TIMER, 1000, None) } as i32,
        "Start update polling timer",
    )?;
    app.refresh_updates()
}

pub fn self_test() -> Result<(), String> {
    native_checks()
}

fn native_checks() -> Result<(), String> {
    let path = std::env::temp_dir().join(format!(
        "CursorHighlighter.Desktop-{}.json",
        std::process::id()
    ));
    let foreground = unsafe { GetForegroundWindow() };
    let app = Box::new(App::new(Settings::default(), path.clone()));
    CURRENT.with(|current| current.set(&*app));
    let result = (|| {
        let mut count = 0;
        let mut ensure = |condition: bool, description: &str| -> Result<(), String> {
            if !condition {
                return Err(format!("Native check failed: {description}"));
            }
            count += 1;
            Ok(())
        };
        app.setup(false)?;
        unsafe {
            let style = GetWindowLongPtrW(app.halo.get(), GWL_EXSTYLE) as u32;
            let required = WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_NOACTIVATE
                | WS_EX_TOOLWINDOW
                | WS_EX_TOPMOST;
            ensure(
                style & required == required,
                "click-through, topmost and non-activation styles",
            )?;
            ensure(
                GetForegroundWindow() == foreground,
                "showing halo preserves focus",
            )?;
            ensure(
                IsWindowVisible(app.halo.get()) != 0,
                "enabled halo is visible",
            )?;
            ensure(
                !app.hook.get().is_null(),
                "native low-level mouse hook installed",
            )?;
            ensure(
                app.keyboard_hook.get().is_null(),
                "default behavior does not observe keyboard activity",
            )?;
            ensure(app.tray_added.get(), "notification icon registered")?;
            ensure(
                IsWindowVisible(app.window.get()) == 0,
                "hidden startup leaves Settings hidden with a usable tray",
            )?;
            ensure(
                SendMessageW(app.control(ID_START_MINIMIZED), BM_GETCHECK, 0, 0) == 0
                    && SendMessageW(app.control(ID_LAUNCH_AT_LOGIN), BM_GETCHECK, 0, 0) == 0,
                "startup controls are opt-in",
            )?;
            SendMessageW(app.control(ID_START_MINIMIZED), BM_CLICK, 0, 0);
            ensure(
                app.settings.borrow().start_minimized,
                "native hidden-startup preference notification",
            )?;
            SendMessageW(app.control(ID_START_MINIMIZED), BM_CLICK, 0, 0);
            ensure(
                GetWindowLongPtrW(app.control(ID_COLOR), GWL_STYLE) as u32 & 0xF
                    == BS_OWNERDRAW as u32,
                "color preview uses native owner-drawn button",
            )?;
            for color in ["#FFD43B", "#123456", "#FFFFFF", "#000000"] {
                app.settings.borrow_mut().color_hex = color.into();
                app.update_labels()?;
                for state in [0, ODS_SELECTED, ODS_FOCUS] {
                    verify_color_button(&app, state)?;
                    ensure(
                        true,
                        "color button paints selected color and contrasting text in normal/pressed/focused states",
                    )?;
                }
            }
            app.settings.borrow_mut().color_hex = Settings::default().color_hex;
            app.update_labels()?;
            for kind in [ICON_SMALL, ICON_BIG] {
                ensure(
                    SendMessageW(app.window.get(), WM_GETICON, kind as usize, 0) != 0,
                    "custom window icon loaded",
                )?;
            }
            let mut cursor: POINT = zeroed();
            check(GetCursorPos(&mut cursor), "Get test cursor")?;
            let mut rect: RECT = zeroed();
            check(
                GetWindowRect(app.halo.get(), &mut rect),
                "Get test halo bounds",
            )?;
            ensure(
                rect.right - rect.left == 84 && rect.bottom - rect.top == 84,
                "default diameter",
            )?;
            ensure(
                (rect.left + 42 - cursor.x).abs() <= 3 && (rect.top + 42 - cursor.y).abs() <= 3,
                "actual cursor centering",
            )?;
            ensure(
                WindowFromPoint(cursor) != app.halo.get(),
                "hit test passes through halo",
            )?;
            ensure(
                SendMessageW(app.halo.get(), WM_MOUSEACTIVATE, 0, 0) == MA_NOACTIVATE as isize,
                "mouse activation denied",
            )?;
            app.settings.borrow_mut().diameter = 150;
            app.changed()?;
            check(
                GetWindowRect(app.halo.get(), &mut rect),
                "Get resized halo bounds",
            )?;
            ensure(rect.right - rect.left == 154, "live native resize")?;
            app.toggle()?;
            ensure(IsWindowVisible(app.halo.get()) == 0, "disable hides halo")?;
            app.toggle()?;
            ensure(IsWindowVisible(app.halo.get()) != 0, "reenable shows halo")?;
            ensure(
                GetForegroundWindow() == foreground,
                "updates preserve focus",
            )?;

            app.toggle()?;
            for (id, preset) in [
                (ID_PRESET_SLIDES, PresentationPreset::Slides),
                (ID_PRESET_DEMO, PresentationPreset::LiveDemo),
            ] {
                SendMessageW(app.control(id), BM_CLICK, 0, 0);
                let mut expected = Settings {
                    enabled: false,
                    ..Settings::default()
                };
                expected.apply_preset(preset);
                ensure(
                    *app.settings.borrow() == expected,
                    "preset button changes appearance without enabling halo",
                )?;
                for (slider, value) in [
                    (ID_DIAMETER, expected.diameter),
                    (ID_OPACITY, expected.opacity),
                ] {
                    ensure(
                        SendMessageW(app.control(slider), TBM_GETPOS, 0, 0) as u32 == value,
                        "preset synchronizes manual sliders",
                    )?;
                }
                app.save();
                ensure(
                    Settings::load(&path)? == expected,
                    "preset values persist in existing settings file",
                )?;
                ensure(
                    IsWindowVisible(app.halo.get()) == 0,
                    "preset leaves disabled halo hidden",
                )?;
            }
            app.toggle()?;
            app.command(ID_PRESET_SLIDES)?;
            check(
                GetWindowRect(app.halo.get(), &mut rect),
                "Get preset halo bounds",
            )?;
            ensure(
                rect.right - rect.left == 64 && rect.bottom - rect.top == 64,
                "preset immediately resizes enabled halo",
            )?;
            ensure(
                GetForegroundWindow() == foreground,
                "preset updates preserve focus",
            )?;
            SendMessageW(app.control(ID_DIAMETER), TBM_SETPOS, 1, 120);
            SendMessageW(
                app.window.get(),
                WM_HSCROLL,
                0,
                app.control(ID_DIAMETER) as isize,
            );
            SendMessageW(app.control(ID_OPACITY), TBM_SETPOS, 1, 50);
            SendMessageW(
                app.window.get(),
                WM_HSCROLL,
                0,
                app.control(ID_OPACITY) as isize,
            );
            ensure(
                app.settings.borrow().diameter == 120 && app.settings.borrow().opacity == 50,
                "native slider notifications",
            )?;
            SendMessageW(app.control(ID_SCALE_WITH_DPI), BM_CLICK, 0, 0);
            ensure(
                app.settings.borrow().scale_with_dpi,
                "native DPI sizing checkbox notification",
            )?;
            check(GetCursorPos(&mut cursor), "Get DPI sizing test cursor")?;
            let diameter = app
                .settings
                .borrow()
                .pixel_diameter(app.cursor_dpi(cursor)?)?;
            check(
                GetWindowRect(app.halo.get(), &mut rect),
                "Get DPI-scaled halo bounds",
            )?;
            ensure(
                rect.right - rect.left == diameter as i32 + 4
                    && rect.bottom - rect.top == diameter as i32 + 4,
                "DPI-scaled halo bitmap uses the cursor monitor",
            )?;
            app.save();
            ensure(
                Settings::load(&path)?.scale_with_dpi,
                "DPI sizing choice persists",
            )?;
            SendMessageW(app.control(ID_SCALE_WITH_DPI), BM_CLICK, 0, 0);
            check(
                GetWindowRect(app.halo.get(), &mut rect),
                "Get physical-pixel halo bounds",
            )?;
            ensure(
                !app.settings.borrow().scale_with_dpi && rect.right - rect.left == 124,
                "unchecking DPI sizing immediately restores physical pixels",
            )?;
            SendMessageW(app.control(ID_ENABLED), BM_CLICK, 0, 0);
            ensure(
                !app.settings.borrow().enabled,
                "native checkbox notification",
            )?;
            SendMessageW(app.control(ID_OUTLINE), BM_CLICK, 0, 0);
            ensure(
                app.settings.borrow().outline_only,
                "outline checkbox notification",
            )?;
            app.save();
            ensure(
                Settings::load(&path)?.outline_only,
                "outline checkbox choice persists",
            )?;
            SendMessageW(app.control(ID_PRESET_SLIDES), BM_CLICK, 0, 0);
            ensure(
                app.settings.borrow().outline_only
                    && SendMessageW(app.control(ID_OUTLINE), BM_GETCHECK, 0, 0) != 0,
                "presentation preset preserves outline selection",
            )?;
            SendMessageW(app.control(ID_OUTLINE), BM_CLICK, 0, 0);
            ensure(
                !app.settings.borrow().outline_only,
                "outline checkbox toggles back",
            )?;
            ensure(
                app.hotkey_id.get() != 0,
                "global shortcut is registered before testing dispatch",
            )?;
            SendMessageW(app.window.get(), WM_HOTKEY, app.hotkey_id.get() as usize, 0);
            ensure(app.settings.borrow().enabled, "global shortcut dispatch")?;
            SendMessageW(app.control(ID_OUTLINE), BM_CLICK, 0, 0);
            ensure(
                SendMessageW(app.control(ID_CLICK_COLORS), BM_GETCHECK, 0, 0) == 0,
                "distinct click colors default off",
            )?;
            SendMessageW(app.control(ID_CLICK_COLORS), BM_CLICK, 0, 0);
            ensure(
                app.settings.borrow().distinct_click_colors,
                "distinct click colors checkbox notification",
            )?;
            mouse_proc(HC_ACTION as i32, WM_LBUTTONDOWN as usize, 0);
            app.tick()?;
            ensure(app.rendered_colors.get() == [1.0, 0.0], "left click color")?;
            mouse_proc(HC_ACTION as i32, WM_RBUTTONDOWN as usize, 0);
            app.tick()?;
            ensure(
                app.rendered_colors.get() == [1.0; 2],
                "overlapping click colors",
            )?;
            app.toggle()?;
            app.toggle()?;
            ensure(
                app.rendered_colors.get() == [0.0; 2],
                "disable clears click colors",
            )?;
            SendMessageW(app.control(ID_AUTO_HIDE), BM_CLICK, 0, 0);
            ensure(
                app.settings.borrow().auto_hide
                    && app.settings.borrow().outline_only
                    && !app.keyboard_hook.get().is_null(),
                "auto-hide works with outline and installs keyboard activity observer",
            )?;
            let now = app.now();
            app.auto_hide.replace(AutoHide::new(now));
            app.tick_at(now + crate::model::IDLE_HIDE_MS)?;
            ensure(
                app.rendered_alpha.get() == 255,
                "idle delay preserves full opacity before fading",
            )?;
            app.tick_at(now + crate::model::IDLE_HIDE_MS + 90)?;
            ensure(
                app.rendered_alpha.get() == 128,
                "idle fade updates layered-window alpha",
            )?;
            app.tick_at(now + crate::model::IDLE_HIDE_MS + 180)?;
            ensure(
                IsWindowVisible(app.halo.get()) == 0,
                "idle fade hides the halo",
            )?;
            app.auto_hide.replace(AutoHide::new(app.now()));
            keyboard_proc(HC_ACTION as i32, WM_KEYUP as usize, 0);
            app.tick_at(app.now() + 180)?;
            ensure(
                app.rendered_alpha.get() == 255,
                "key release alone does not hide the halo",
            )?;
            app.auto_hide.replace(AutoHide::new(app.now()));
            keyboard_proc(HC_ACTION as i32, WM_KEYDOWN as usize, 0);
            app.tick_at(app.now() + 180)?;
            ensure(
                IsWindowVisible(app.halo.get()) == 0,
                "activity-only keyboard callback fades out the halo",
            )?;
            let now = app.now() + 180;
            app.cursor_position
                .set(Some((cursor.x.wrapping_add(1), cursor.y)));
            let event = MSLLHOOKSTRUCT {
                pt: cursor,
                ..zeroed()
            };
            mouse_proc(
                HC_ACTION as i32,
                WM_MOUSEMOVE as usize,
                &event as *const _ as isize,
            );
            app.tick_at(now + 180)?;
            ensure(
                IsWindowVisible(app.halo.get()) != 0 && app.rendered_alpha.get() == 255,
                "mouse movement restores full opacity",
            )?;
            app.auto_hide.replace(AutoHide::new(app.now()));
            keyboard_proc(HC_ACTION as i32, WM_SYSKEYDOWN as usize, 0);
            app.tick_at(app.now() + 180)?;
            ensure(
                IsWindowVisible(app.halo.get()) == 0,
                "system-key activity also hides the halo",
            )?;
            SendMessageW(app.control(ID_AUTO_HIDE), BM_CLICK, 0, 0);
            ensure(
                !app.settings.borrow().auto_hide
                    && app.keyboard_hook.get().is_null()
                    && app.rendered_alpha.get() == 255
                    && IsWindowVisible(app.halo.get()) != 0,
                "disabling auto-hide removes keyboard observer and restores halo",
            )?;
            SendMessageW(app.control(ID_AUTO_HIDE), BM_CLICK, 0, 0);
            SendMessageW(app.control(ID_OUTLINE), BM_CLICK, 0, 0);
            app.command(ID_HIDE)?;
            ensure(IsWindowVisible(app.window.get()) == 0, "hide to tray")?;
            ensure(
                Settings::load(&path)? == *app.settings.borrow(),
                "UI settings persistence",
            )?;
            SendMessageW(app.control(ID_AUTO_HIDE), BM_CLICK, 0, 0);

            let old_dpi = app.dpi.get();
            for dpi in [96, 144, 192] {
                app.dpi.set(dpi);
                app.layout()?;
                let controls = app.controls.borrow().clone();
                for control in controls {
                    let mut bounds: RECT = zeroed();
                    check(
                        GetWindowRect(control.window, &mut bounds),
                        "Get scaled control bounds",
                    )?;
                    ensure(
                        bounds.right - bounds.left == app.scale(control.width),
                        "DPI-scaled control width",
                    )?;
                    ensure(
                        bounds.bottom - bounds.top == app.scale(control.height),
                        "DPI-scaled control height",
                    )?;
                }
            }
            app.dpi.set(old_dpi);
            app.layout()?;

            app.settings.replace(Settings::default());
            app.settings.borrow_mut().enabled = false;
            app.tick()?;
            std::thread::sleep(std::time::Duration::from_millis(100));
            check(GetCursorPos(&mut cursor), "Get compositing test position")?;
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let mut monitor_info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..zeroed()
            };
            check(
                GetMonitorInfoW(monitor, &mut monitor_info),
                "Get test display bounds",
            )?;
            let x = cursor.x
                + if cursor.x + 12 < monitor_info.rcMonitor.right {
                    12
                } else {
                    -12
                };
            let y = cursor.y
                + if cursor.y + 12 < monitor_info.rcMonitor.bottom {
                    12
                } else {
                    -12
                };
            // Isolate capture from changing presentation/chat content under the cursor.
            let backing = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                SETTINGS_CLASS,
                w!("Cursor Highlighter compositing check"),
                WS_POPUP,
                cursor.x - 70,
                cursor.y - 70,
                140,
                140,
                null_mut(),
                null_mut(),
                app.instance,
                null(),
            );
            if backing.is_null() {
                return Err(os_error("Create stable compositing background"));
            }
            let _backing = TestWindow(backing);
            ShowWindow(backing, SW_SHOWNOACTIVATE);
            UpdateWindow(backing);
            std::thread::sleep(std::time::Duration::from_millis(150));
            let baseline = capture_pixel(x, y)?;
            app.settings.borrow_mut().enabled = true;
            app.tick()?;
            std::thread::sleep(std::time::Duration::from_millis(100));
            let normal = capture_pixel(x, y)?;
            let rgb = app.settings.borrow().rgb()?;
            let alpha = (255.0_f32 * 0.35).round() / 255.0;
            for c in 0..3 {
                let expected = rgb[c] as f32 * alpha + baseline[c] as f32 * (1.0 - alpha);
                ensure(
                    (normal[c] as f32 - expected).abs() <= 8.0,
                    &format!(
                        "actual desktop translucent compositing: baseline {baseline:?}, actual {normal:?}, channel {c} expected {expected}"
                    ),
                )?;
            }
            mouse_proc(HC_ACTION as i32, WM_LBUTTONDOWN as usize, 0);
            app.tick()?;
            ensure(app.rendered.get() == 1.0, "mouse hook press deforms halo")?;
            std::thread::sleep(std::time::Duration::from_millis(100));
            let pressed = capture_pixel(x, y)?;
            for c in 0..3 {
                let expected =
                    (rgb[c] as f32 * 0.65).round() * alpha + baseline[c] as f32 * (1.0 - alpha);
                ensure(
                    (pressed[c] as f32 - expected).abs() <= 8.0,
                    "actual darker click compositing",
                )?;
            }
            ensure(
                WindowFromPoint(cursor) != app.halo.get(),
                "pressed halo remains click-through",
            )?;
            mouse_proc(HC_ACTION as i32, WM_LBUTTONUP as usize, 0);
            std::thread::sleep(std::time::Duration::from_millis(310));
            app.tick()?;
            ensure(
                app.rendered.get() == 0.0,
                "click release restores appearance",
            )?;
            let ring_x = cursor.x
                + if cursor.x + 36 < monitor_info.rcMonitor.right {
                    36
                } else {
                    -36
                };
            app.settings.borrow_mut().enabled = false;
            app.tick()?;
            std::thread::sleep(std::time::Duration::from_millis(100));
            let ring_baseline = capture_pixel(ring_x, cursor.y)?;
            app.settings.borrow_mut().enabled = true;
            app.settings.borrow_mut().outline_only = true;
            app.tick()?;
            std::thread::sleep(std::time::Duration::from_millis(100));
            let outline_center = capture_pixel(x, y)?;
            for c in 0..3 {
                ensure(
                    (outline_center[c] as i16 - baseline[c] as i16).abs() <= 8,
                    "outline keeps the cursor interior clear",
                )?;
            }
            let outline_ring = capture_pixel(ring_x, cursor.y)?;
            let alpha = (255.0_f32 * 0.35).round() / 255.0;
            let mut actual_cursor: POINT = zeroed();
            check(GetCursorPos(&mut actual_cursor), "Get outline test cursor")?;
            let mut outline_rect: RECT = zeroed();
            check(
                GetWindowRect(app.halo.get(), &mut outline_rect),
                "Get outline bounds",
            )?;
            for c in 0..3 {
                let expected = rgb[c] as f32 * alpha + ring_baseline[c] as f32 * (1.0 - alpha);
                ensure(
                    (outline_ring[c] as f32 - expected).abs() <= 8.0,
                    &format!(
                        "outline ring opacity: baseline {ring_baseline:?}, actual {outline_ring:?}, channel {c} expected {expected}, cursor ({}, {}) now ({}, {}), bounds ({}, {}, {}, {}), alpha {}, visible {}",
                        cursor.x,
                        cursor.y,
                        actual_cursor.x,
                        actual_cursor.y,
                        outline_rect.left,
                        outline_rect.top,
                        outline_rect.right,
                        outline_rect.bottom,
                        app.rendered_alpha.get(),
                        IsWindowVisible(app.halo.get())
                    ),
                )?;
            }
            mouse_proc(HC_ACTION as i32, WM_LBUTTONDOWN as usize, 0);
            app.tick()?;
            std::thread::sleep(std::time::Duration::from_millis(100));
            let outline_pressed = capture_pixel(ring_x, cursor.y)?;
            check(
                GetCursorPos(&mut actual_cursor),
                "Get pressed outline test cursor",
            )?;
            for c in 0..3 {
                let expected = (rgb[c] as f32 * 0.65).round() * alpha
                    + ring_baseline[c] as f32 * (1.0 - alpha);
                ensure(
                    (outline_pressed[c] as f32 - expected).abs() <= 8.0,
                    &format!(
                        "outline click feedback: baseline {ring_baseline:?}, actual {outline_pressed:?}, channel {c} expected {expected}, cursor ({}, {}) now ({}, {}), feedback {}, alpha {}",
                        cursor.x,
                        cursor.y,
                        actual_cursor.x,
                        actual_cursor.y,
                        app.rendered.get(),
                        app.rendered_alpha.get()
                    ),
                )?;
            }
            mouse_proc(HC_ACTION as i32, WM_LBUTTONUP as usize, 0);
            std::thread::sleep(std::time::Duration::from_millis(310));
            app.tick()?;
            check(
                PostMessageW(app.window.get(), WM_CLOSE, 0, 0),
                "Request native window close",
            )?;
            let mut saw_quit = false;
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_QUIT {
                    saw_quit = true;
                    break;
                }
                if IsDialogMessageW(app.window.get(), &message) == 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            ensure(
                saw_quit,
                "posted close through the real keyboard/message loop terminates app",
            )?;
            ensure(
                IsWindowVisible(app.halo.get()) == 0,
                "closing hides overlay",
            )?;
        }
        println!(
            "PASS: {count} native Win32 checks (controls, DPI layout, halo, icons, click feedback, focus, compositing, persistence)."
        );
        Ok(())
    })();
    CURRENT.with(|current| current.set(null()));
    drop(app);
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    result
}

unsafe fn verify_color_button(app: &App, state: u32) -> Result<(), String> {
    unsafe {
        let screen = GetDC(null_mut());
        if screen.is_null() {
            return Err(os_error("Get color preview test DC"));
        }
        let memory = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, 190, 33);
        if memory.is_null() || bitmap.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            if !memory.is_null() {
                DeleteDC(memory);
            }
            ReleaseDC(null_mut(), screen);
            return Err(os_error("Create color preview test bitmap"));
        }
        let previous = SelectObject(memory, bitmap);
        let result = (|| {
            let item = DRAWITEMSTRUCT {
                CtlType: ODT_BUTTON,
                CtlID: ID_COLOR as u32,
                itemAction: ODA_DRAWENTIRE,
                itemState: state,
                hwndItem: app.control(ID_COLOR),
                hDC: memory,
                rcItem: RECT {
                    left: 0,
                    top: 0,
                    right: 190,
                    bottom: 33,
                },
                ..zeroed()
            };
            if SendMessageW(
                app.window.get(),
                WM_DRAWITEM,
                ID_COLOR as usize,
                &item as *const _ as isize,
            ) != 1
            {
                return Err("Color preview WM_DRAWITEM was not handled.".into());
            }
            let [r, g, b] = app.settings.borrow().rgb()?;
            let expected = r as u32 | ((g as u32) << 8) | ((b as u32) << 16);
            if GetPixel(memory, 10, 10) != expected {
                return Err(
                    "Color preview background does not match the selected halo color.".into(),
                );
            }
            let text = if r.min(g).min(b) as u32 + r.max(g).max(b) as u32 >= 255 {
                0
            } else {
                0xFFFFFF
            };
            let mut matching = 0;
            for y in 8..25 {
                for x in 20..170 {
                    matching += usize::from(GetPixel(memory, x, y) == text);
                }
            }
            if matching < 10 {
                return Err("Color preview is missing contrasting text.".into());
            }
            Ok(())
        })();
        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(null_mut(), screen);
        result
    }
}

unsafe fn capture_pixel(x: i32, y: i32) -> Result<[u8; 3], String> {
    unsafe {
        let screen = GetDC(null_mut());
        if screen.is_null() {
            return Err(os_error("Capture screen DC"));
        }
        let memory = CreateCompatibleDC(screen);
        if memory.is_null() {
            ReleaseDC(null_mut(), screen);
            return Err(os_error("Capture memory DC"));
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 1,
                biHeight: -1,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..zeroed()
            },
            ..zeroed()
        };
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(memory, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bitmap.is_null() {
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
            return Err(os_error("Create capture bitmap"));
        }
        let previous = SelectObject(memory, bitmap);
        let result = if previous.is_null() || previous == GDI_ERROR as HGDIOBJ {
            Err(os_error("Select capture bitmap"))
        } else {
            let result = check(
                BitBlt(memory, 0, 0, 1, 1, screen, x, y, SRCCOPY | CAPTUREBLT),
                "Capture desktop pixel",
            )
            .and_then(|_| {
                check(GdiFlush(), "Flush desktop capture")?;
                let bytes = std::slice::from_raw_parts(bits.cast::<u8>(), 4);
                Ok([bytes[2], bytes[1], bytes[0]])
            });
            SelectObject(memory, previous);
            result
        };
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(null_mut(), screen);
        result
    }
}

#[cfg(test)]
mod activation_tests {
    use super::*;

    static NATIVE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn explicit_hold_overrides_auto_hide_without_changing_toggle_fades() {
        let mut settings = Settings {
            auto_hide: true,
            ..Settings::default()
        };
        let mut hide = AutoHide::new(0);
        hide.typed(0);
        assert_eq!(halo_alpha(&settings, &mut hide, 180), 0);
        settings.activation_mode = ActivationMode::Hold;
        assert_eq!(halo_alpha(&settings, &mut hide, 180), 255);
        assert_eq!(halo_alpha(&settings, &mut hide, 5000), 255);
        settings.activation_mode = ActivationMode::Toggle;
        assert_eq!(halo_alpha(&settings, &mut hide, 5000), 0);
        hide.moved(5000);
        assert_eq!(halo_alpha(&settings, &mut hide, 5180), 255);
        settings.auto_hide = false;
        assert_eq!(halo_alpha(&settings, &mut hide, 10000), 255);
    }

    #[test]
    fn halo_centering_handles_negative_coordinates_and_simulated_dpi_transitions() {
        let settings = Settings {
            diameter: 81,
            scale_with_dpi: true,
            ..Settings::default()
        };
        for (x, y, dpi, diameter) in [
            (100, 100, 96, 81),
            (-1920, -1080, 144, 122),
            (-1, 0, 192, 162),
            (0, -1, 120, 101),
            (0, 0, 96, 81),
        ] {
            assert_eq!(settings.pixel_diameter(dpi).unwrap(), diameter);
            let size = diameter as i32 + 4;
            let position = halo_position(POINT { x, y }, size);
            assert_eq!(position.x, x - size / 2);
            assert_eq!(position.y, y - size / 2);
            // Odd raster dimensions center on the same pixel, within half a pixel.
            assert!((position.x as f64 + size as f64 / 2.0 - x as f64).abs() <= 0.5);
            assert!((position.y as f64 + size as f64 / 2.0 - y as f64).abs() <= 0.5);
        }
    }

    unsafe extern "system" fn collect_monitors(
        _: HMONITOR,
        _: HDC,
        rect: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        unsafe {
            let monitors = &mut *(data as *mut Vec<RECT>);
            monitors.push(*rect);
        }
        1
    }

    #[test]
    fn dpi_probe_tracks_attached_monitors_without_showing_or_taking_focus() {
        let _lock = NATIVE_TEST_LOCK.lock().unwrap();
        let app = App::new(Settings::default(), PathBuf::new());
        unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(halo_proc),
                hInstance: app.instance,
                lpszClassName: HALO_CLASS,
                ..zeroed()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let settings_class = WNDCLASSW {
                lpfnWndProc: Some(settings_proc),
                hInstance: app.instance,
                lpszClassName: SETTINGS_CLASS,
                ..zeroed()
            };
            assert_ne!(RegisterClassW(&settings_class), 0);
            let foreground = GetForegroundWindow();
            let common = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
            };
            assert_ne!(InitCommonControlsEx(&common), 0);
            app.window.set(CreateWindowExW(
                WS_EX_CONTROLPARENT,
                SETTINGS_CLASS,
                w!("Hidden settings layout check"),
                WS_POPUP,
                0,
                0,
                450,
                830,
                null_mut(),
                null_mut(),
                app.instance,
                null(),
            ));
            assert!(!app.window.get().is_null());
            app.add_controls().unwrap();
            app.update_labels().unwrap();
            for dpi in [96, 144, 192] {
                app.dpi.set(dpi);
                app.layout().unwrap();
                let mut previous_bottom = 0;
                for id in [
                    ID_PRESET_SLIDES,
                    ID_SIZE_LABEL,
                    ID_DIAMETER,
                    ID_SCALE_WITH_DPI,
                    ID_OPACITY_LABEL,
                    ID_OPACITY,
                    ID_OUTLINE,
                    ID_COLOR,
                    ID_CLICK_COLORS,
                    ID_AUTO_HIDE,
                    ID_START_MINIMIZED,
                    ID_LAUNCH_AT_LOGIN,
                    ID_SHORTCUT,
                    ID_HOLD,
                    ID_STATUS,
                    ID_HIDE,
                ] {
                    let control = app.control(id);
                    assert!(!control.is_null());
                    assert_eq!(GetDlgCtrlID(control), id as i32);
                    let mut bounds: RECT = zeroed();
                    assert_ne!(GetWindowRect(control, &mut bounds), 0);
                    assert!(bounds.top >= previous_bottom, "overlapping control {id}");
                    previous_bottom = bounds.bottom;
                    assert!(bounds.bottom <= app.scale(830));
                }
                let controls = app.controls.borrow();
                for (index, control) in controls.iter().enumerate() {
                    assert!(control.x >= 0 && control.y >= 0);
                    assert!(control.x + control.width <= 450);
                    assert!(control.y + control.height <= 830);
                    for other in controls.iter().skip(index + 1) {
                        assert!(
                            control.x + control.width <= other.x
                                || other.x + other.width <= control.x
                                || control.y + control.height <= other.y
                                || other.y + other.height <= control.y,
                            "settings controls overlap"
                        );
                    }
                }
            }
            app.dpi.set(96);
            assert_eq!(IsWindowVisible(app.window.get()), 0);
            let mut monitors = Vec::<RECT>::new();
            assert_ne!(
                EnumDisplayMonitors(
                    null_mut(),
                    null(),
                    Some(collect_monitors),
                    &mut monitors as *mut Vec<RECT> as isize,
                ),
                0
            );
            assert!(!monitors.is_empty());
            for rect in monitors.iter().chain(monitors.iter().rev()) {
                for cursor in [
                    POINT {
                        x: rect.left,
                        y: rect.top,
                    },
                    POINT {
                        x: rect.right - 1,
                        y: rect.bottom - 1,
                    },
                ] {
                    let dpi = app.cursor_dpi(cursor).unwrap();
                    assert!(dpi > 0);
                    assert_ne!(
                        AreDpiAwarenessContextsEqual(
                            GetWindowDpiAwarenessContext(app.dpi_probe.get()),
                            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                        ),
                        0
                    );
                    assert_eq!(
                        MonitorFromWindow(app.dpi_probe.get(), MONITOR_DEFAULTTONEAREST),
                        MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
                    );
                    let mut bounds = zeroed();
                    assert_ne!(GetWindowRect(app.dpi_probe.get(), &mut bounds), 0);
                    assert_eq!(bounds.right - bounds.left, 1);
                    assert_eq!(bounds.bottom - bounds.top, 1);
                    assert_eq!(IsWindowVisible(app.dpi_probe.get()), 0);
                    assert_eq!(GetForegroundWindow(), foreground);
                    assert_eq!(app.dpi.get(), 96);
                }
            }
        }
    }

    #[test]
    fn hold_requires_every_configured_key_and_accepts_either_windows_key() {
        let shortcut = Shortcut::parse("Ctrl + Alt + Shift + Win + F13").unwrap();
        let keys = [
            shortcut.key as i32,
            VK_CONTROL as i32,
            VK_MENU as i32,
            VK_SHIFT as i32,
            VK_LWIN as i32,
        ];
        assert!(chord_held(&shortcut, |key| keys.contains(&key)));
        for released in keys {
            assert!(!chord_held(&shortcut, |key| {
                keys.contains(&key) && key != released
            }));
        }
        assert!(chord_held(&shortcut, |key| {
            key == VK_RWIN as i32 || (keys.contains(&key) && key != VK_LWIN as i32)
        }));
        let default = Shortcut::default();
        assert!(chord_held(&default, |key| {
            assert!([b'H' as i32, VK_CONTROL as i32, VK_MENU as i32].contains(&key));
            true
        }));
    }

    struct ReservedHotkey {
        window: HWND,
        id: i32,
    }

    impl Drop for ReservedHotkey {
        fn drop(&mut self) {
            assert_ne!(unsafe { UnregisterHotKey(self.window, self.id) }, 0);
        }
    }

    struct CurrentApp;
    impl Drop for CurrentApp {
        fn drop(&mut self) {
            CURRENT.with(|current| current.set(null()));
        }
    }

    #[test]
    fn hidden_updates_dialog_opt_out_dpi_and_install_gate_without_network_or_real_settings() {
        let _lock = NATIVE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        let app = Box::new(App::new(
            Settings {
                enabled: false,
                ..Settings::default()
            },
            path.clone(),
        ));
        unsafe {
            let common = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
            };
            assert_ne!(InitCommonControlsEx(&common), 0);
            let class = WNDCLASSW {
                lpfnWndProc: Some(settings_proc),
                hInstance: app.instance,
                lpszClassName: SETTINGS_CLASS,
                ..zeroed()
            };
            assert_ne!(RegisterClassW(&class), 0);
            app.window.set(CreateWindowExW(
                0,
                SETTINGS_CLASS,
                w!("Hidden update test"),
                WS_OVERLAPPED,
                0,
                0,
                450,
                830,
                null_mut(),
                null_mut(),
                app.instance,
                null(),
            ));
            assert!(!app.window.get().is_null());
            app.add_controls().unwrap();
            CURRENT.with(|current| current.set(&*app));
            let _current = CurrentApp;
            let foreground = GetForegroundWindow();
            app.updates_dialog(false).unwrap();
            assert_eq!(IsWindowVisible(app.updates_window.get()), 0);
            assert_eq!(GetForegroundWindow(), foreground);
            assert_eq!(
                SendMessageW(app.control(ID_UPDATE_AUTO), BM_GETCHECK, 0, 0),
                BST_CHECKED as isize
            );
            assert_eq!(IsWindowEnabled(app.control(ID_UPDATE_INSTALL)), 0);
            assert!(!app.updates.borrow().busy());
            assert!(!app.updates.borrow().installing);
            for dpi in [96, 144, 192] {
                app.layout_updates(dpi).unwrap();
                for control in app.updates_controls.borrow().iter() {
                    let mut bounds: RECT = zeroed();
                    assert_ne!(GetWindowRect(control.window, &mut bounds), 0);
                    assert_eq!(
                        bounds.right - bounds.left,
                        (control.width * dpi as i32 + 48) / 96
                    );
                    assert_eq!(
                        bounds.bottom - bounds.top,
                        (control.height * dpi as i32 + 48) / 96
                    );
                }
            }
            SendMessageW(app.control(ID_UPDATE_AUTO), BM_CLICK, 0, 0);
            assert!(!app.settings.borrow().automatic_updates);
            app.save();
            assert!(!Settings::load(&path).unwrap().automatic_updates);
            assert!(!app.updates.borrow().busy());
            app.updates.borrow_mut().installing = true;
            app.refresh_updates().unwrap();
            assert_eq!(IsWindowEnabled(app.window.get()), 0);
            assert_eq!(IsWindowEnabled(app.control(ID_UPDATE_CHECK)), 0);
            app.updates.borrow_mut().installing = false;
            app.refresh_updates().unwrap();
            assert_ne!(IsWindowEnabled(app.window.get()), 0);
            SendMessageW(app.updates_window.get(), WM_CLOSE, 0, 0);
            assert_eq!(IsWindowVisible(app.updates_window.get()), 0);
            assert_eq!(GetForegroundWindow(), foreground);
            assert!(app.fatal.borrow().is_none());
            app.dirty.set(false);
        }
    }

    #[test]
    fn native_shortcut_controls_conflicts_replacement_and_hold_dispatch() {
        let _lock = NATIVE_TEST_LOCK.lock().unwrap();
        let app = Box::new(App::new(
            Settings {
                enabled: false,
                ..Settings::default()
            },
            std::env::temp_dir().join("CursorHighlighter.UnusedActivationTest.json"),
        ));
        unsafe {
            let common = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
            };
            assert_ne!(InitCommonControlsEx(&common), 0);
            let class = WNDCLASSW {
                lpfnWndProc: Some(settings_proc),
                hInstance: app.instance,
                lpszClassName: SETTINGS_CLASS,
                ..zeroed()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let window = CreateWindowExW(
                0,
                SETTINGS_CLASS,
                w!("Activation test"),
                WS_OVERLAPPED,
                0,
                0,
                450,
                570,
                null_mut(),
                null_mut(),
                app.instance,
                null(),
            );
            assert!(!window.is_null());
            app.window.set(window);
            app.add_controls().unwrap();
            let blocker = TestWindow(CreateWindowExW(
                0,
                w!("STATIC"),
                w!("Shortcut reservation test"),
                WS_OVERLAPPED,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                app.instance,
                null(),
            ));
            assert!(!blocker.0.is_null());
            CURRENT.with(|current| current.set(&*app));
            let _current = CurrentApp;

            app.update_labels().unwrap();
            assert_eq!(
                SendMessageW(app.control(ID_CLICK_COLORS), BM_GETCHECK, 0, 0),
                0
            );
            SendMessageW(app.control(ID_CLICK_COLORS), BM_CLICK, 0, 0);
            assert!(app.settings.borrow().distinct_click_colors);
            SendMessageW(app.control(ID_CLICK_COLORS), BM_CLICK, 0, 0);
            assert!(!app.settings.borrow().distinct_click_colors);
            app.dirty.set(false);

            // Use unused extended function-key chords, never the running app's default.
            let initial = (13..=24)
                .find_map(|number| {
                    let shortcut =
                        Shortcut::parse(&format!("Ctrl + Alt + Shift + F{number}")).unwrap();
                    app.register_shortcut(&shortcut).ok().map(|()| shortcut)
                })
                .expect("an unused Ctrl+Alt+Shift+F13-F24 chord");
            app.settings.borrow_mut().shortcut = initial.clone();
            let modifiers = MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT;
            let conflict = (13..=24)
                .find_map(|number| {
                    let shortcut =
                        Shortcut::parse(&format!("Ctrl + Alt + Shift + F{number}")).unwrap();
                    (shortcut != initial
                        && RegisterHotKey(blocker.0, 99, modifiers, shortcut.key) != 0)
                        .then_some(shortcut)
                })
                .expect("another unused extended function-key chord");
            let reservation = ReservedHotkey {
                window: blocker.0,
                id: 99,
            };

            let original_id = app.hotkey_id.get();
            app.text(ID_SHORTCUT, &conflict.label()).unwrap();
            SendMessageW(window, WM_COMMAND, ID_APPLY_SHORTCUT as usize, 0);
            assert_eq!(app.settings.borrow().shortcut, initial);
            assert_eq!(app.hotkey_id.get(), original_id);
            assert!(!app.dirty.get());
            let mut status = [0u16; 1024];
            let length = GetWindowTextW(app.control(ID_STATUS), status.as_mut_ptr(), 1024);
            let status = String::from_utf16_lossy(&status[..length as usize]);
            assert!(status.contains("unavailable") && status.contains("unchanged"));
            assert_eq!(RegisterHotKey(blocker.0, 100, modifiers, initial.key), 0);

            app.text(ID_SHORTCUT, "Ctrl + Space").unwrap();
            SendMessageW(window, WM_COMMAND, ID_APPLY_SHORTCUT as usize, 0);
            assert_eq!(app.settings.borrow().shortcut, initial);
            assert!(!app.dirty.get());

            drop(reservation);
            app.text(ID_SHORTCUT, &conflict.label()).unwrap();
            SendMessageW(window, WM_COMMAND, ID_APPLY_SHORTCUT as usize, 0);
            assert_eq!(app.settings.borrow().shortcut, conflict);
            assert_ne!(app.hotkey_id.get(), original_id);
            assert!(app.dirty.get());
            assert_ne!(RegisterHotKey(blocker.0, 100, modifiers, initial.key), 0);
            let _released = ReservedHotkey {
                window: blocker.0,
                id: 100,
            };
            let replacement_id = app.hotkey_id.get();
            SendMessageW(window, WM_COMMAND, ID_APPLY_SHORTCUT as usize, 0);
            assert_eq!(app.hotkey_id.get(), replacement_id);

            SendMessageW(app.control(ID_HOLD), BM_CLICK, 0, 0);
            assert_eq!(app.settings.borrow().activation_mode, ActivationMode::Hold);
            app.dirty.set(false);
            SendMessageW(window, WM_HOTKEY, replacement_id as usize, 0);
            assert!(!app.settings.borrow().enabled);
            assert!(!app.hold_active.get());
            assert!(
                !app.dirty.get(),
                "hold dispatch must not persist temporary state"
            );
            SendMessageW(window, WM_HOTKEY, original_id as usize, 0);
            assert!(
                !app.settings.borrow().enabled,
                "ignore messages for the old registration"
            );
            SendMessageW(app.control(ID_HOLD), BM_CLICK, 0, 0);
            assert_eq!(
                app.settings.borrow().activation_mode,
                ActivationMode::Toggle
            );
            app.settings.borrow_mut().enabled = true;
            app.dirty.set(false);
            SendMessageW(window, WM_HOTKEY, replacement_id as usize, 0);
            assert!(
                !app.settings.borrow().enabled,
                "toggle shortcut changes enabled state"
            );
            assert!(app.dirty.get(), "toggle changes remain persisted");
            assert!(app.fatal.borrow().is_none());
            assert_eq!(IsWindowVisible(window), 0);
        }
    }
}
