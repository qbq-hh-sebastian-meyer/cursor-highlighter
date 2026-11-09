# Cursor Highlighter

A small **Rust application with a native Win32 UI** for Windows 10/11. It puts a
colored, translucent halo around the actual desktop cursor across normal desktop
applications. No screen dimming, click interception, browser/webview, .NET runtime,
administrator privileges or background service. Launch at Windows login is
available as an explicit opt-in; nothing is registered by default.

## Download

Published versions are available on the repository's **Releases** page. Download
`cursor-highlighter-windows-arm64.zip` for Windows on ARM or
`cursor-highlighter-windows-x64.zip` for Intel/AMD Windows, extract the archive,
and launch `cursor-highlighter.exe`. No Rust or .NET installation is needed.
Each archive has a matching `.sha256` file. The executables are not code-signed;
Windows SmartScreen may show an unknown-publisher warning.

## Run

Double-click `Run.cmd` in `C:\Projects\cursor-highlighter` to build and launch.
To launch the already-built version without any development tools:

```powershell
& 'C:\Projects\cursor-highlighter\target\release\cursor-highlighter.exe'
```

The release executable includes the icon, DPI manifest, and statically linked
Rust/C runtime. Copy just this executable to another Windows machine of the same
architecture. This machine builds **ARM64**; build for x64 on an x64 development
machine or use the MSVC cross-compilation instructions below.

## Controls

- **Highlight cursor** enables or disables the halo.
- **Presentation presets:** select **Slides** for a subtle 60-unit yellow
  (`#FFD43B`) halo at 20% opacity, or **Live demo** for a stronger 120-unit orange
  (`#FF922B`) halo at 50% opacity. Both are also available from the tray menu
  without opening Settings. Presets replace size, opacity, and color immediately,
  but preserve enabled, auto-hide, and DPI sizing choices. Size uses the selected
  physical or logical units. You can still customize all three controls
  afterward; the resulting values are saved normally, with no preset field.
- **Size** adjusts the diameter from 24 to 240 physical screen pixels by default.
- **Scale size with monitor DPI** switches Size to logical pixels at 96 DPI.
  The halo follows the Windows scaling of the monitor under the cursor: an
  80-unit diameter renders as 80 pixels at 100%, 120 at 150%, and 160 at 200%.
  This keeps its perceived size consistent with other Windows UI across laptop
  and external displays. Uncheck it to restore fixed physical-pixel sizing.
  The Size label identifies the current units; the saved size value does not
  change when switching modes. Windows scaling, not measured panel dimensions,
  determines the result.
- **Opacity** adjusts transparency from 5% to 80%.
- **Choose...** previews the current halo color on the button, with contrasting
  text, and opens the standard Windows color picker.
- **Click feedback:** left, right, and middle presses slightly squash the halo and
  darken the selected color. It stays pressed during a drag and smoothly returns
  to normal after release. Even quick clicks remain visible briefly. Input is
  always forwarded to the underlying application.
- **Auto-hide when idle (1.5 s) or typing** is optional and off by default. When
  enabled, the halo fades out over 180 ms after 1.5 seconds without mouse
  movement, or as soon as keyboard activity is detected. Moving the mouse fades
  it back in over 180 ms. Typing keeps it hidden until the mouse moves, and the
  Highlight cursor toggle still takes priority. Keyboard detection observes only
  key-down activity: key identities and contents are not read or stored. The
  keyboard observer is installed only while this option is enabled.
- **Ctrl + Alt + H** toggles the halo even while Settings is hidden. If another
  application owns the shortcut, Settings shows a warning; the checkbox and tray
  menu still work.
- **Hide to tray** leaves the halo running. Double-click the notification icon or
  right-click it and select **Settings** to reopen the window.
- **Start hidden in the tray** skips showing Settings on subsequent launches;
  the halo and notification icon still run. Default: off.
- **Launch at Windows login (current user)** registers this executable for your
  Windows account, without administrator privileges. Default: off. Turn it off
  to remove the entry. This is independent of starting hidden: enable both for
  quiet startup at login.
- **Exit**, the tray's **Exit**, or closing Settings stops the app. Minimizing
  only minimizes Settings.

Defaults: 80 physical px diameter (DPI scaling off), yellow (`#FFD43B`),
35% opacity, auto-hide off.
Settings changes apply immediately and save after a 400 ms debounce, on hide,
and on exit to `%LOCALAPPDATA%\CursorHighlighter\settings.json`.
**Existing settings from the previous C# version are preserved**: the file path
and existing PascalCase JSON fields have not changed. The new `AutoHide` boolean
defaults to `false` when absent from older files. Corrupt/unreadable files show a
warning and use defaults. Save failures show a warning while changes remain
active in memory. Settings are written to a temporary file then atomically
replaced.
The optional `ScaleWithDpi` JSON boolean persists the DPI sizing choice. Older
settings files without this field retain physical-pixel sizing.

Launch-at-login registration uses only the `CursorHighlighter` value in
`HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, with the full
executable path quoted. Its checkbox reflects whether that entry exists rather
than a potentially stale JSON preference. Registration changes only when you
click the checkbox, never when loading or saving settings. Errors are shown and
the checkbox is restored on failure; if the entry cannot be read, the control is
disabled until restart. Windows may delay startup or disable it through Task
Manager's Startup apps settings. Exit does not unregister it. Keep the executable
in a stable location; turn the option off before moving/deleting it, then enable
it again from the new location. Paths exceeding Windows' 260-character Run
command limit cannot be registered.

Only one app instance is allowed per Windows session. Exit any older C# build
before starting the Rust build; they intentionally share the same instance guard.

## Build

Install these once:

1. Rust stable using [rustup](https://rustup.rs/), with the native **MSVC** host
   toolchain (`aarch64-pc-windows-msvc` on ARM64, `x86_64-pc-windows-msvc` on x64).
2. Visual Studio 2022 Build Tools with **Desktop development with C++**, the
   Windows 11 SDK, and the C++ compiler/libraries for your target architecture.
   ARM64 builds also need **MSVC ARM64/ARM64EC build tools**.

Then:

```powershell
Set-Location 'C:\Projects\cursor-highlighter'
.\Build.cmd
```

`Build.cmd` adds `%USERPROFILE%\.cargo\bin` to its own PATH, so it also works after
the toolchain installation in this session without restarting the shell.
In a shell where Cargo is already on PATH, the equivalent is:

```powershell
cargo build --release --locked
```

The Windows SDK resource compiler embeds `assets\CursorHighlighter.ico` and
`app.manifest` via `build.rs`. The custom icon is used for the executable,
settings window, taskbar, and notification area. To regenerate its seven
resolutions (16-256 px):

```powershell
powershell -ExecutionPolicy Bypass -File assets\Generate-Icon.ps1
```

To cross-compile for x64, install the x64 MSVC libraries/tools and SDK support:

```powershell
rustup target add x86_64-pc-windows-msvc
cargo build --release --locked --target x86_64-pc-windows-msvc
```

The resulting executable is `target\x86_64-pc-windows-msvc\release\cursor-highlighter.exe`.
Cross-compilation requires both the host toolchain's native libraries (for build
scripts) and the target libraries. There is no installer/publishing step.
Cargo downloads dependencies on the first build; the app itself has no network
functionality.

## GitHub Actions releases

The **Windows release** workflow builds and unit-tests on native Windows x64 and
ARM64 runners. It also checks formatting, runs Clippy, verifies the executable's
PE architecture, and uploads a ZIP plus SHA-256 checksum for each architecture.
It runs for pushes to `main`, pull requests, version tags, and manually through
**Actions > Windows release > Run workflow**. Workflow artifacts are retained for
30 days. Interactive desktop self-tests are deliberately not run on hosted CI;
run them locally as described below.

To publish a GitHub Release, update `version` in `Cargo.toml`, regenerate
`Cargo.lock` with `cargo check`, commit and push those changes, then push a
matching version tag:

```powershell
git tag v0.1.0
git push origin v0.1.0
```

Replace `v0.1.0` with the package version being released. A tag must exactly match
`v` followed by the Cargo package version. After both builds pass, the workflow
creates a GitHub Release with generated notes and attaches both architecture
packages and their checksums. Untagged/manual runs build artifacts without
publishing a release.

## Validation

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo run --locked -- --self-test
```

Unit tests cover C# settings compatibility, validation, atomic persistence,
presentation preset values, persistence and subsequent manual customization,
click/release/overlapping-button timing, idle/typing fade timing and movement
restoration, auto-hide persistence and backward-compatible defaults, alpha, color
darkening, deformation, and premultiplied rendering across sizes and opacity values.
DPI tests cover
opt-in defaults, persistence in both modes, fractional-scale rounding, simulated
monitor transitions, larger scaled bitmaps, and negative-coordinate centering.
A hidden-window test checks DPI lookup and monitor selection on attached displays
without moving the real cursor, showing an overlay, taking focus, installing
hooks, or changing user settings. Simulated transitions and hidden-window checks
do not establish physical mixed-DPI cursor behavior.
Startup tests cover default-off JSON compatibility and quoted executable commands
(including spaces, Unicode, UNC paths, and invalid/overlong input). Tests never
register the app for login or change real user settings.

`--self-test` additionally creates real native windows and exercises the
notification icon, hook installation, custom window icons, click-through and
no-activation behavior, cursor centering, topmost placement, native controls,
hotkey dispatch, optional keyboard observer lifecycle, idle/typing hiding and
mouse-movement restoration, settings persistence, DPI-scaled control layout,
live DPI sizing checkbox changes, and
actual desktop compositing for both the normal and pressed halo. It uses an isolated
temporary settings file, never overwriting your settings. Close the running app
first, keep the desktop unlocked, and leave the cursor still over a static area
while running this check. It calls the mouse-observer callback directly to check
click feedback and calls the keyboard observer with activity-only messages; it
does not inject real clicks or keystrokes into your desktop.

Before a presentation, also verify interactively:

1. Move the pointer across all monitors, including mixed-DPI displays. Check both
   sizing modes, monitor boundaries, and displays left of or above the primary
   display; repeat while holding a mouse button and after changing display scale.
2. Click, drag, scroll, and type in the applications you plan to present.
3. Check the audience's view or a test recording in your sharing application.

## Implementation and limitations

`windows-sys` provides direct Win32 bindings; there is no GUI framework.
`src\desktop.rs` owns the native windows, controls, tray, timers, hook, color
dialog, shortcut, and GDI resources on one UI thread. `src\model.rs` contains
settings, click/auto-hide timing, and anti-aliased premultiplied BGRA rendering.
`src\startup.rs` owns the current-user launch-at-login registry entry.

The halo is a per-pixel-alpha, click-through, non-activating layered topmost
window. A 16 ms timer follows desktop cursor coordinates; only appearance changes
regenerate its bitmap. The low-level mouse hook observes button transitions and
movement; the optional low-level keyboard hook observes activity without reading
key data. Both always forward input without rendering inside the callback.
Auto-hide fades use the layered window's constant alpha, preserving the selected
color, opacity, and click feedback. Per-monitor V2 DPI
awareness scales Settings on monitor changes. Halo dimensions remain fixed in
physical pixels by default. When DPI sizing is enabled, a hidden, non-activating
1-pixel window selects the cursor's monitor and `GetDpiForWindow` reads its current
effective DPI. This avoids selecting a monitor based on the larger halo's area
when it straddles a boundary. The frame timer regenerates the bitmap when the
scaled diameter changes, including while click feedback is active. DPI lookup
and positioning use the same cursor sample. Negative desktop coordinates are
supported; the halo clips naturally at display edges.

**Screen sharing capture is application- and mode-dependent.** Sharing an entire
display often captures desktop overlays; sharing a single application may omit
the halo. There is no guarantee of capture in every sharing/recording mode.
Exclusive full-screen applications, other topmost overlays, and protected content
may obscure it. The app does not draw on the Windows secure desktop
(UAC/sign-in/lock screens) and is not designed for remote/headless sessions.
Actual mixed-monitor interactions and audience capture are not established by
the automated checks.
