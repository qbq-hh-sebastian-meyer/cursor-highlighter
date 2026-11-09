use crate::model::{ClickFeedback, Settings, render};
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
const ID_CLICK_COLORS: u16 = 111;
const FRAME_TIMER: usize = 1;
const SAVE_TIMER: usize = 2;
const TRAY_MESSAGE: u32 = WM_APP + 1;
const TBM_GETPOS: u32 = WM_USER;
const HALO_CLASS: *const u16 = w!("CursorHighlighter.Rust.Halo");
const SETTINGS_CLASS: *const u16 = w!("CursorHighlighter.Rust.Settings");
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
    hook: Cell<HHOOK>,
    font: Cell<HFONT>,
    controls: RefCell<Vec<Control>>,
    settings: RefCell<Settings>,
    settings_path: PathBuf,
    click: RefCell<ClickFeedback>,
    start: Instant,
    rendered: Cell<f32>,
    rendered_colors: Cell<[f32; 2]>,
    appearance_dirty: Cell<bool>,
    dirty: Cell<bool>,
    tray_added: Cell<bool>,
    hotkey_registered: Cell<bool>,
    fatal: RefCell<Option<String>>,
    dpi: Cell<u32>,
    taskbar_message: Cell<u32>,
}

impl App {
    fn new(settings: Settings, settings_path: PathBuf) -> Self {
        Self {
            instance: unsafe { GetModuleHandleW(null()) },
            window: Cell::new(null_mut()),
            halo: Cell::new(null_mut()),
            hook: Cell::new(null_mut()),
            font: Cell::new(null_mut()),
            controls: RefCell::new(Vec::new()),
            settings: RefCell::new(settings),
            settings_path,
            click: RefCell::new(ClickFeedback::default()),
            start: Instant::now(),
            rendered: Cell::new(-1.0),
            rendered_colors: Cell::new([0.0; 2]),
            appearance_dirty: Cell::new(true),
            dirty: Cell::new(false),
            tray_added: Cell::new(false),
            hotkey_registered: Cell::new(false),
            fatal: RefCell::new(None),
            dpi: Cell::new(96),
            taskbar_message: Cell::new(0),
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
                bottom: self.scale(490),
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
            self.hotkey_registered.set(
                RegisterHotKey(window, 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, b'H' as u32) != 0,
            );
            let shortcut_status = if self.hotkey_registered.get() {
                "Ctrl + Alt + H toggles the halo.\r\nChanges save automatically. Closing this window exits.".into()
            } else {
                format!(
                    "Shortcut unavailable ({}).\r\nUse the checkbox or tray menu to toggle.",
                    std::io::Error::last_os_error()
                )
            };
            self.text(ID_STATUS, &shortcut_status)?;
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
        unsafe { GetDlgItem(self.window.get(), id as i32) }
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
        self.add_control(w!("STATIC"), "", 0, ID_SIZE_LABEL, 24, 125, 390, 24)?;
        let diameter = self.add_control(
            TRACKBAR_CLASSW,
            "",
            TBS_AUTOTICKS | WS_TABSTOP,
            ID_DIAMETER,
            24,
            153,
            398,
            35,
        )?;
        self.add_control(w!("STATIC"), "", 0, ID_OPACITY_LABEL, 24, 202, 390, 24)?;
        let opacity = self.add_control(
            TRACKBAR_CLASSW,
            "",
            TBS_AUTOTICKS | WS_TABSTOP,
            ID_OPACITY,
            24,
            230,
            398,
            35,
        )?;
        self.add_control(w!("STATIC"), "Halo color", 0, 0, 24, 286, 90, 24)?;
        self.add_control(
            w!("BUTTON"),
            "",
            WS_TABSTOP | BS_OWNERDRAW as u32,
            ID_COLOR,
            124,
            278,
            190,
            33,
        )?;
        self.add_control(
            w!("BUTTON"),
            "Distinct clicks: blue left / orange right",
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
            ID_CLICK_COLORS,
            24,
            324,
            400,
            25,
        )?;
        self.add_control(w!("STATIC"), "", 0, ID_STATUS, 24, 370, 400, 60)?;
        self.add_control(
            w!("BUTTON"),
            "Hide to tray",
            WS_TABSTOP,
            ID_HIDE,
            24,
            441,
            115,
            32,
        )?;
        self.add_control(w!("BUTTON"), "Exit", WS_TABSTOP, ID_EXIT, 150, 441, 85, 32)?;
        let settings = self.settings.borrow().clone();
        unsafe {
            SendMessageW(enabled, BM_SETCHECK, usize::from(settings.enabled), 0);
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
            &format!("Size: {} px diameter", settings.diameter),
        )?;
        self.text(ID_OPACITY_LABEL, &format!("Opacity: {}%", settings.opacity))?;
        self.text(ID_COLOR, &format!("{}   Choose...", settings.color_hex))?;
        unsafe {
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
        match id {
            ID_ENABLED => {
                let enabled =
                    unsafe { SendMessageW(self.control(ID_ENABLED), BM_GETCHECK, 0, 0) != 0 };
                self.settings.borrow_mut().enabled = enabled;
                self.changed()?;
            }
            ID_COLOR => self.choose_color()?,
            ID_CLICK_COLORS => {
                let enabled =
                    unsafe { SendMessageW(self.control(ID_CLICK_COLORS), BM_GETCHECK, 0, 0) != 0 };
                self.settings.borrow_mut().distinct_click_colors = enabled;
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
        {
            let mut settings = self.settings.borrow_mut();
            settings.enabled = !settings.enabled;
        }
        self.changed()
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
        let settings = self.settings.borrow().clone();
        if !settings.enabled {
            self.click.borrow_mut().reset();
            unsafe {
                ShowWindow(self.halo.get(), SW_HIDE);
            }
            self.appearance_dirty.set(true);
            return Ok(());
        }
        let now = self.now();
        let amount = self.click.borrow().amount(now);
        let colors = if settings.distinct_click_colors {
            self.click.borrow().color_amounts(now)
        } else {
            [0.0; 2]
        };
        if self.appearance_dirty.get()
            || self.rendered.get() != amount
            || self.rendered_colors.get() != colors
        {
            let pixels = render(&settings, amount, colors)?;
            self.upload(&pixels, settings.diameter as i32 + 4)?;
            self.rendered.set(amount);
            self.rendered_colors.set(colors);
            self.appearance_dirty.set(false);
        }
        unsafe {
            let mut cursor = zeroed();
            check(GetCursorPos(&mut cursor), "Get desktop cursor position")?;
            let size = settings.diameter as i32 + 4;
            check(
                SetWindowPos(
                    self.halo.get(),
                    HWND_TOPMOST,
                    cursor.x - size / 2,
                    cursor.y - size / 2,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                ),
                "Position halo",
            )?;
        }
        Ok(())
    }

    fn upload(&self, pixels: &[u8], size: i32) -> Result<(), String> {
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
                let mut cursor = zeroed();
                let result =
                    check(GetCursorPos(&mut cursor), "Get halo upload position").and_then(|_| {
                        let position = POINT {
                            x: cursor.x - size / 2,
                            y: cursor.y - size / 2,
                        };
                        let dimensions = SIZE { cx: size, cy: size };
                        let blend = BLENDFUNCTION {
                            BlendOp: AC_SRC_OVER as u8,
                            SourceConstantAlpha: 255,
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
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            if !self.window.get().is_null() {
                KillTimer(self.window.get(), FRAME_TIMER);
                KillTimer(self.window.get(), SAVE_TIMER);
            }
            if !self.hook.get().is_null() && UnhookWindowsHookEx(self.hook.get()) == 0 {
                eprintln!("{}", os_error("Remove mouse hook"));
            }
            if self.hotkey_registered.get() {
                UnregisterHotKey(self.window.get(), 1);
            }
            if self.tray_added.get() {
                Shell_NotifyIconW(NIM_DELETE, &self.tray_data());
            }
            if !self.halo.get().is_null() {
                DestroyWindow(self.halo.get());
            }
            if IsWindow(self.window.get()) != 0 {
                DestroyWindow(self.window.get());
            }
            if !self.font.get().is_null() {
                DeleteObject(self.font.get());
            }
            UnregisterClassW(HALO_CLASS, self.instance);
            UnregisterClassW(SETTINGS_CLASS, self.instance);
        }
    }
}

unsafe extern "system" fn mouse_proc(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code >= 0 {
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

unsafe extern "system" fn halo_proc(window: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_NCHITTEST => HTTRANSPARENT as isize,
        // Keep physical-pixel diameter across monitor DPI transitions.
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
            WM_COMMAND => app.command((wp & 0xFFFF) as u16),
            WM_HSCROLL => {
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
            WM_HOTKEY if wp == 1 => app.toggle(),
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
    let app = Box::new(App::new(settings, path));
    CURRENT.with(|current| current.set(&*app));
    let result = (|| {
        app.setup(true)?;
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
                if IsDialogMessageW(app.window.get(), &message) == 0 {
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
            ensure(app.tray_added.get(), "notification icon registered")?;
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
            SendMessageW(app.control(ID_ENABLED), BM_CLICK, 0, 0);
            ensure(
                !app.settings.borrow().enabled,
                "native checkbox notification",
            )?;
            SendMessageW(app.window.get(), WM_HOTKEY, 1, 0);
            ensure(app.settings.borrow().enabled, "global shortcut dispatch")?;
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
            app.command(ID_HIDE)?;
            ensure(IsWindowVisible(app.window.get()) == 0, "hide to tray")?;
            ensure(
                Settings::load(&path)? == *app.settings.borrow(),
                "UI settings persistence",
            )?;

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
