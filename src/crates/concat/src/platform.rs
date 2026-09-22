// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! What the window asks of the platform it runs on.
//!
//! Four things differ between a desk and a phone: how the backend is
//! chosen, how a file or folder is picked, whether the window has a title
//! strip of its own to drag, and whether a file dragged in from outside the
//! window is even a thing that can happen. Everything else in the crate is
//! the same tree, the same state and the same callbacks, so the differences
//! live here and nowhere else.
//!
//! Desktop and iOS draw through the winit backend; Android through Slint's
//! android-activity backend, which the activity sets up before [`crate::run`]
//! is called. File dialogs are the desktop's: on a phone a pick goes through
//! the system's document picker, which arrives with the phone layout. A drag
//! in from the OS is a desktop thing for the same reason: winit only reports
//! `DroppedFile` on macOS, Windows and X11 - not Wayland, which has no such
//! event as of this winit, and not iOS, which has no such gesture.

use std::path::PathBuf;
#[cfg(not(target_os = "android"))]
use std::sync::atomic::{AtomicU64, Ordering};

use slint::PlatformError;
// The winit backend, and so these, exist everywhere but Android, which
// draws through Slint's android-activity backend and has no winit at all.
#[cfg(not(target_os = "android"))]
use slint::winit_030::winit::event::WindowEvent;
#[cfg(not(target_os = "android"))]
use slint::winit_030::winit::event_loop::ActiveEventLoop;
#[cfg(not(target_os = "android"))]
use slint::winit_030::winit::window::{Window as WinitWindow, WindowId};
#[cfg(not(target_os = "android"))]
use slint::winit_030::{CustomApplicationHandler, EventResult};

use crate::gpu::Gpu;

/// The physical pixels of the last monitor a window was known to be on,
/// `(width << 32) | height`, or 0 for "not seen yet".
///
/// A process-wide cell rather than a field on [`DropHandler`] because the
/// reader - [`monitor_size`] - is a free function with no handle to the
/// handler: `select_backend` takes the handler and winit keeps it, while
/// the launch form calls `monitor_size` from elsewhere. `AtomicU64` over an
/// `Rc<Cell<..>>` because it needs no ownership to reach and no thread to
/// agree on: it is a plain const-initialised static, which is the shape
/// `FILE_PICKER` below already uses for process-wide platform state.
#[cfg(not(target_os = "android"))]
static MONITOR: AtomicU64 = AtomicU64::new(0);

/// Packs a measured monitor into [`MONITOR`]'s cell.
#[cfg(not(target_os = "android"))]
fn pack_monitor(size: (u32, u32)) -> u64 {
    (u64::from(size.0) << 32) | u64::from(size.1)
}

/// Unpacks [`MONITOR`]'s cell; 0 - never seen - is `None`. Desktop only:
/// it is read by [`monitor_size`], whose phone branch has no window to read
/// a monitor from.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn unpack_monitor(bits: u64) -> Option<(u32, u32)> {
    (bits != 0).then(|| ((bits >> 32) as u32, bits as u32))
}

/// Collects the paths of a single OS drag as `DroppedFile` events deliver
/// them one at a time, then hands the whole batch to `on_dropped` once
/// winit says this pass over the event queue is done - the same shape a
/// picked-files dialog hands the caller, so the caller need not know drag
/// and drop split it up.
///
/// It is also the only place the window's monitor is seen: the launch form
/// is published before `App::run` maps a window, so `monitor_size` has
/// nothing to read then, and this handler - handed the winit window on its
/// every event - is where the real measurement comes from. `on_monitor`
/// fires when that measurement changes, so the caller can re-publish.
#[cfg(not(target_os = "android"))]
struct DropHandler {
    pending: Vec<PathBuf>,
    on_dropped: Box<dyn Fn(Vec<PathBuf>)>,
    on_monitor: Box<dyn Fn()>,
    seen_monitor: bool,
}

#[cfg(not(target_os = "android"))]
impl DropHandler {
    fn new(
        on_dropped: impl Fn(Vec<PathBuf>) + 'static,
        on_monitor: impl Fn() + 'static,
    ) -> Self {
        Self {
            pending: Vec::new(),
            on_dropped: Box::new(on_dropped),
            on_monitor: Box::new(on_monitor),
            seen_monitor: false,
        }
    }

    /// Records the monitor of the window this event came from, and calls
    /// back only when it differs from what was last recorded - a resize, a
    /// move to another screen or a scale change is a new measurement, and
    /// every other event is a chance to take the first one. A window with
    /// no monitor yet (unmapped, hidden) records nothing, so the fallback
    /// survives until a real size is known.
    fn learn_monitor(&mut self, window: Option<&WinitWindow>) {
        let Some(size) = window
            .and_then(|window| window.current_monitor())
            .map(|monitor| {
                let size = monitor.size();
                (size.width, size.height)
            })
        else {
            return;
        };
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        let bits = pack_monitor(size);
        let previous = MONITOR.swap(bits, Ordering::Relaxed);
        if !self.seen_monitor || previous != bits {
            self.seen_monitor = true;
            (self.on_monitor)();
        }
    }
}

#[cfg(not(target_os = "android"))]
impl CustomApplicationHandler for DropHandler {
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        winit_window: Option<&WinitWindow>,
        _slint_window: Option<&slint::Window>,
        event: &WindowEvent,
    ) -> EventResult {
        match event {
            WindowEvent::DroppedFile(path) => self.pending.push(path.clone()),
            // The events that can change which monitor the window is on.
            WindowEvent::Resized(_)
            | WindowEvent::Moved(_)
            | WindowEvent::ScaleFactorChanged { .. } => self.learn_monitor(winit_window),
            // ...and the first event of any kind, so the measurement is
            // taken as soon as a window exists to take it from.
            _ if !self.seen_monitor => self.learn_monitor(winit_window),
            _ => {}
        }
        EventResult::Propagate
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) -> EventResult {
        if !self.pending.is_empty() {
            (self.on_dropped)(std::mem::take(&mut self.pending));
        }
        EventResult::Propagate
    }
}

/// Whether the window draws the macOS traffic lights over its own strip.
pub const MACOS: bool = cfg!(target_os = "macos");

/// Chooses and installs the backend, and hands back the device the
/// renderer and the engine's compositor share, when there is one.
///
/// `on_files_dropped` fires on the event-loop thread with the paths of a
/// file (or several) dragged in from outside the window - Finder, Explorer,
/// a file manager - batched into one call per drag. It is taken here,
/// before the window exists, because the backend - and the hook into its
/// event loop that OS drops arrive through - has to be selected before
/// anything is built on top of it; see [`DropHandler`].
///
/// `on_monitor_changed` fires on that same thread when the window's monitor
/// is first known or changes. The window is where the monitor is measured
/// (see [`monitor_size`]), and it does not exist yet at this point, so the
/// caller gets told when it does and can refresh what it published.
#[cfg(not(target_os = "android"))]
pub fn select_backend(
    on_files_dropped: impl Fn(Vec<PathBuf>) + 'static,
    on_monitor_changed: impl Fn() + 'static,
) -> Result<Option<Gpu>, PlatformError> {
    // The device the renderer and the monitor share. Taken first, because
    // the backend is selected with it.
    let gpu = Gpu::acquire();
    match &gpu {
        None => log::warn!("no GPU adapter; the monitor composites on the CPU"),
        // A software adapter is refused by Slint's selector unless this
        // variable says otherwise: it enumerates the adapters on the instance
        // it was handed, finds none that is a GPU, and `select` fails with
        // "no GPU-backed WGPU adapter is available". From a double-clicked
        // GUI build that failure went to a console nobody could see, and the
        // app just never appeared. Setting it here is what the reporter did
        // by hand, and the window, the editing and the export all worked on
        // WARP - so the app does it for them and says so in the log.
        // https://github.com/jub0t/Concat/issues/135
        Some(gpu) if gpu.is_software() => {
            log::warn!("the GPU adapter is a software rasteriser; the window will render slowly");
            // SAFETY: the process is single-threaded at this point - the
            // backend, the engine's threads and the window all come after
            // this function - so nothing reads the environment concurrently.
            unsafe { std::env::set_var("SLINT_WGPU_CPU", "1") };
        }
        Some(_) => {}
    }

    let mut selector = slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_custom_application_handler(DropHandler::new(
            on_files_dropped,
            on_monitor_changed,
        ));
    selector = match &gpu {
        Some(gpu) => selector.require_wgpu_29(gpu.configuration()),
        None => {
            // Without a shared device, ask for the platform's own API by
            // name: Skia picks its surface from a cfg chain, and requiring
            // one turns a silent fall back to the CPU rasteriser into a
            // refusal to start, which is a fault you can see.
            #[cfg(target_vendor = "apple")]
            {
                selector.require_metal()
            }
            #[cfg(target_family = "windows")]
            {
                selector.require_d3d()
            }
            #[cfg(not(any(target_vendor = "apple", target_family = "windows")))]
            {
                selector
            }
        }
    };

    // The custom title bar. The window draws its own strip, so the
    // platform's is not wanted - but each platform is asked in its own way.
    //
    // macOS keeps the real title bar and makes it invisible: transparent,
    // untitled, with the content view under it. That is what keeps the
    // traffic lights, which are the window's and not ours to draw, and the
    // strip leaves 80px for them (title-bar.slint).
    //
    // Everywhere else the decorations go entirely and the strip carries its
    // own minimise, maximise and close. On Windows winit keeps WS_SIZEBOX
    // when the caption goes, so the edges still resize, and the undecorated
    // shadow keeps the DWM drop shadow the caption would otherwise have
    // taken with it.
    //
    // The attributes below are the window's first state; what keeps the
    // decorations off is the Slint window's `no-frame` (app.slint), which
    // the winit backend re-applies after the window is made. On Wayland
    // without it GNOME's bar came back above the strip:
    // https://github.com/jub0t/Concat/issues/97
    // https://github.com/jub0t/Concat/issues/145
    #[cfg(target_os = "macos")]
    {
        use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;
        selector = selector.with_winit_window_attributes_hook(|attributes| {
            attributes
                .with_titlebar_transparent(true)
                .with_title_hidden(true)
                .with_fullsize_content_view(true)
        });
    }
    #[cfg(target_os = "windows")]
    {
        use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
        selector = selector.with_winit_window_attributes_hook(|attributes| {
            attributes
                .with_decorations(false)
                .with_undecorated_shadow(true)
        });
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "android")))]
    {
        selector = selector
            .with_winit_window_attributes_hook(|attributes| attributes.with_decorations(false));
    }
    selector.select()?;
    Ok(gpu)
}

/// Tells the user the window could not start, in a dialog of the platform's
/// own, because the console the error would otherwise go to is not one a
/// double-clicked build has: on Windows a GUI subsystem binary has no
/// standard error, and the app was exiting with the reason written to
/// nowhere. The log file is named so the reader has something to attach.
/// https://github.com/jub0t/Concat/issues/135
pub fn report_startup_failure(error: &str) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let mut description = format!("Concat could not start.\n\n{error}");
        if let Some(path) = concat_host::logs::current() {
            description.push_str(&format!("\n\nLog: {}", path.display()));
        }
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Concat")
            .set_description(description)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let _ = error;
}

/// Whether the strip should draw its own window buttons: everywhere the
/// platform's decorations were taken off, which is everywhere but macOS,
/// where the traffic lights stay the window's.
pub const OWN_WINDOW_BUTTONS: bool = !MACOS;

/// Minimises the window: the strip's first button.
pub fn minimize(window: &slint::Window) {
    #[cfg(not(target_os = "android"))]
    {
        use slint::winit_030::WinitWindowAccessor;
        window.with_winit_window(|window| {
            window.set_minimized(true);
        });
    }
    #[cfg(target_os = "android")]
    let _ = window;
}

/// Whether the window is currently maximised, for the strip to pick the
/// maximise or the restore glyph. False where there is no such state.
pub fn is_maximized(window: &slint::Window) -> bool {
    #[cfg(not(target_os = "android"))]
    {
        use slint::winit_030::WinitWindowAccessor;
        window
            .with_winit_window(|window| window.is_maximized())
            .unwrap_or(false)
    }
    #[cfg(target_os = "android")]
    {
        let _ = window;
        false
    }
}

/// On Android the activity installed the backend before calling in, and
/// that backend draws on a device of its own; the monitor composites on the
/// CPU and hands the renderer finished pixels. Android has no OS drag to
/// wire up - a file arrives through the document picker instead - so
/// `on_files_dropped` is taken only to keep the signature the same as the
/// desktop's and is never called. `on_monitor_changed` is the same: Android
/// has no winit window to measure a monitor from, so it is never called
/// either.
#[cfg(target_os = "android")]
pub fn select_backend(
    on_files_dropped: impl Fn(Vec<PathBuf>) + 'static,
    on_monitor_changed: impl Fn() + 'static,
) -> Result<Option<Gpu>, PlatformError> {
    let _ = on_files_dropped;
    let _ = on_monitor_changed;
    Ok(None)
}

/// Starts a window drag from the title strip.
pub fn begin_drag(window: &slint::Window) {
    #[cfg(not(target_os = "android"))]
    {
        use slint::winit_030::WinitWindowAccessor;
        window.with_winit_window(|window| {
            let _ = window.drag_window();
        });
    }
    #[cfg(target_os = "android")]
    let _ = window;
}

/// Maximises the window, or restores it: the title strip's double-click.
pub fn toggle_maximize(window: &slint::Window) {
    #[cfg(not(target_os = "android"))]
    {
        use slint::winit_030::WinitWindowAccessor;
        window.with_winit_window(|window| {
            window.set_maximized(!window.is_maximized());
        });
    }
    #[cfg(target_os = "android")]
    let _ = window;
}

/// Asks for a folder, starting at `start` when there is one.
pub fn pick_folder(title: &str, start: &str) -> Option<PathBuf> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if !start.is_empty() {
            dialog = dialog.set_directory(start);
        }
        dialog.pick_folder()
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = (title, start);
        None
    }
}

/// What a phone does when asked for files: shows the system's picker and
/// calls back, later, with what was chosen - an empty list for nothing.
/// Installed by the phone's own crate before the window runs; see
/// [`install_file_picker`].
#[cfg(any(target_os = "android", target_os = "ios"))]
pub type FilePicker = Box<dyn Fn(Box<dyn FnOnce(Vec<PathBuf>) + Send>) + Send + Sync>;

#[cfg(any(target_os = "android", target_os = "ios"))]
static FILE_PICKER: std::sync::OnceLock<FilePicker> = std::sync::OnceLock::new();

/// Installs the picker a phone answers [`pick_files_async`] with. Once;
/// a second call is ignored.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn install_file_picker(picker: FilePicker) {
    let _ = FILE_PICKER.set(picker);
}

/// Asks for files and calls `on_picked` with them, on whichever thread the
/// platform answers from - the caller hops to the window's thread itself.
///
/// On a desktop the dialog blocks and the callback runs before this
/// returns. On a phone the system's picker is another screen: this returns
/// at once and the callback comes when the picker is dismissed, through
/// the picker the phone's crate installed. `filter` is the desktop
/// dialog's; a phone's picker offers every kind of media on its own.
pub fn pick_files_async(
    title: &str,
    filter: Option<(&str, &[&str])>,
    on_picked: impl FnOnce(Vec<PathBuf>) + Send + 'static,
) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        if let Some(paths) = pick_files(title, filter) {
            on_picked(paths);
        }
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = (title, filter);
        match FILE_PICKER.get() {
            Some(picker) => picker(Box::new(on_picked)),
            None => log::warn!("no file picker on this platform yet"),
        }
    }
}

/// Asks for files. `filter` names a family and its extensions, and limits
/// the dialog to them.
pub fn pick_files(title: &str, filter: Option<(&str, &[&str])>) -> Option<Vec<PathBuf>> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if let Some((name, extensions)) = filter {
            dialog = dialog.add_filter(name, extensions);
        }
        dialog.pick_files()
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = (title, filter);
        None
    }
}

/// Shows a written file in the platform's file manager.
///
/// A phone has no file manager to hand a path to, and says so rather than
/// appearing to work: a control that silently does nothing is worse than one
/// that explains itself. The path is in the message, which is the part a
/// developer on a cable can still use.
pub fn reveal(path: &str) -> Result<(), String> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        opener::reveal(path).map_err(|error| error.to_string())
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        Err(format!(
            "this device has no file manager to open {path} with"
        ))
    }
}

/// What a new project's frame should be when Auto is chosen: the
/// monitor's physical pixels, kept as-is when they are plausible; the
/// nearest standard `rungs` entry when they are not; and the first rung
/// — 1080p — when there is no monitor to measure at all.
///
/// The rung list is the launch ladder's own, so an off-ladder size such
/// as a 3440×1440 ultrawide stays exact, while a mis-measured size —
/// a hidden window's default, a tearing-down monitor — is snapped back
/// to something a video project can be.
pub fn auto_resolution(monitor: Option<(u32, u32)>, rungs: &[(u32, u32)]) -> (u32, u32) {
    let fallback = rungs.first().copied().unwrap_or((1920, 1080));
    let Some((width, height)) = monitor else {
        return fallback;
    };
    // No measure at all is the same as none: an unmapped or hidden window
    // reports a zero size, and 1080p is the floor either way.
    if width == 0 || height == 0 {
        return fallback;
    }
    // A size with no room for a real desktop is a mis-measurement, not a
    // monitor. Snap it to the nearest standard rung rather than let a
    // stray 640×200 become a 640×200 project.
    if width < 960 || height < 540 {
        return rungs
            .iter()
            .min_by_key(|(w, h)| {
                let dw = i64::from(*w) - i64::from(width);
                let dh = i64::from(*h) - i64::from(height);
                dw * dw + dh * dh
            })
            .copied()
            .unwrap_or(fallback);
    }
    (width, height)
}

/// The physical pixels of the window's current monitor, when the platform
/// can say. `None` before the window is mapped, on a phone, or on a
/// platform without winit.
///
/// The measurement taken from a real window, by the event handler, comes
/// first: the launch form is published before `App::run` maps a window, so
/// the accessor has nothing to read at that moment and the handler's record
/// is the only measurement there is. The accessor is the fallback, for a
/// window the handler has not seen an event from yet.
pub fn monitor_size(window: &slint::Window) -> Option<(u32, u32)> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use slint::winit_030::WinitWindowAccessor;
        let size = unpack_monitor(MONITOR.load(Ordering::Relaxed)).or_else(|| {
            window
                .with_winit_window(|window| {
                    window.current_monitor().map(|monitor| {
                        let size = monitor.size();
                        (size.width, size.height)
                    })
                })
                .flatten()
        });
        log_monitor(window, size);
        size
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = window;
        None
    }
}

/// One line per change, not per call: the resolved monitor and whether the
/// Slint window has a winit window behind it yet. The launch readout is
/// published before `App::run` maps one, so a run's first line records the
/// `None` that produced the fallback rung, and the next records what the
/// window actually has - which is how a stale readout is told from a
/// correct one. It lives here rather than on one caller because both the
/// publish path and the create a user clicks through read the monitor
/// through this function.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn log_monitor(window: &slint::Window, size: Option<(u32, u32)>) {
    use slint::winit_030::WinitWindowAccessor;
    static LOGGED: AtomicU64 = AtomicU64::new(0);
    // 0 is "nothing logged yet"; `None` and a real size are told apart by
    // the top bit, which a packed (width, height) cannot set on its own
    // for a monitor a desk would have.
    let bits = match size {
        None => 1,
        Some(size) => pack_monitor(size) | (1 << 63),
    };
    if LOGGED.swap(bits, Ordering::Relaxed) != bits {
        log::info!(
            "monitor: has_winit_window={} size={size:?}",
            window.has_winit_window()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::auto_resolution;

    /// The creation ladder, as it stands in studio.rs; a local copy so this
    /// module stays a pure function of its arguments.
    const RUNGS: [(u32, u32); 5] = [
        (1920, 1080),
        (1280, 720),
        (3840, 2160),
        (2560, 1440),
        (1080, 1920),
    ];

    #[test]
    fn a_plausible_monitor_stays_exact() {
        assert_eq!(auto_resolution(Some((2560, 1440)), &RUNGS), (2560, 1440));
        assert_eq!(auto_resolution(Some((3440, 1440)), &RUNGS), (3440, 1440));
    }

    #[test]
    fn no_monitor_is_the_1080p_rung() {
        assert_eq!(auto_resolution(None, &RUNGS), (1920, 1080));
        assert_eq!(auto_resolution(Some((0, 0)), &RUNGS), (1920, 1080));
    }

    #[test]
    fn an_implausible_size_snaps_to_the_nearest_rung() {
        assert_eq!(auto_resolution(Some((640, 200)), &RUNGS), (1280, 720));
    }
}
