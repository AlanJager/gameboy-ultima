// objc 0.2's `sel!` checks `cfg(feature = "cargo-clippy")` at this crate.
#![allow(unexpected_cfgs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use gameboy_ultima::link::{normalize_link_name, LinkHub, LinkSeat};
use gameboy_ultima::{
    audio_pace, rgb555_to_888, step_quiet, AudioPlay, Bus, Registers, CYCLES_PER_FRAME, HEIGHT,
    WIDTH,
};
#[path = "controls.rs"]
mod controls;
use controls::*;
#[cfg(target_os = "macos")]
#[path = "metal_screen.rs"]
mod metal_screen;

use gpui::{
    actions, canvas, div, img, point, prelude::*, px, rgb, size, App, Application, Bounds, Context,
    FocusHandle, KeyBinding, KeyDownEvent, KeyUpEvent, Menu, MenuItem, PathPromptOptions, Pixels,
    Render, RenderImage, Window, WindowBackgroundAppearance, WindowBounds, WindowOptions,
};
use image::{Frame as ImageFrame, ImageBuffer, Rgba};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BG: u32 = 0x0B1012;
const PANEL_HI: u32 = 0x202A2C;
const TEXT: u32 = 0xE7E8DE;
const MUTED: u32 = 0x98A19C;
const ACCENT: u32 = 0xC8F169;
const BOOT_ANIMATION_REVEAL_DURATION: Duration = Duration::from_millis(1800);
const BOOT_ANIMATION_HOLD_DURATION: Duration = Duration::from_millis(3000);

fn boot_animation_duration() -> Duration {
    BOOT_ANIMATION_REVEAL_DURATION + BOOT_ANIMATION_HOLD_DURATION
}
const SCREENSHOT_FRAME_DELAY: u32 = 30;
const COVER_WIDTH: u32 = 480;
const COVER_HEIGHT: u32 = 640;
const COVER_ACCENT_TOP: u32 = 92;
const COVER_ACCENT_HEIGHT: u32 = 8;
const COVER_SCREEN_TOP: u32 = 100;

actions!(
    ultima,
    [
        OpenRom,
        ChooseCover,
        ShowLibrary,
        ShowGame,
        TogglePause,
        ToggleBiosHle,
        SaveSlot1,
        SaveSlot2,
        SaveSlot3,
        LoadSlot1,
        LoadSlot2,
        LoadSlot3,
        QuickSaveState,
        QuickLoadState,
        VolumeUp,
        VolumeDown,
        ToggleMute,
        ResumeState,
        StartFresh,
        ResetConsole,
        ShowControls,
        ShowLink,
        ToggleMetalScreen,
        ToggleSharpenScreen,
        ToggleCpuScreen,
        ToggleLinearScreen,
        CompareScreens,
        Quit
    ]
);

#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = ultima, no_json)]
struct SetLockedSpeed(u8);

#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = ultima, no_json)]
struct SetTurboSpeed(u8);

#[derive(Clone, Copy, PartialEq, Eq)]
enum PictureMode {
    #[cfg(target_os = "macos")]
    Metal,
    #[cfg(target_os = "macos")]
    Sharpen,
    CpuNearest,
    Linear,
}

impl PictureMode {
    fn title(self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            PictureMode::Metal => "Metal 最近邻",
            #[cfg(target_os = "macos")]
            PictureMode::Sharpen => "锐化",
            PictureMode::CpuNearest => "CPU 整数最近邻",
            PictureMode::Linear => "线性放大",
        }
    }

    fn order(self) -> u8 {
        match self {
            #[cfg(target_os = "macos")]
            PictureMode::Metal => 0,
            #[cfg(target_os = "macos")]
            PictureMode::Sharpen => 1,
            PictureMode::CpuNearest => 2,
            PictureMode::Linear => 3,
        }
    }

    #[cfg(target_os = "macos")]
    fn metal_filter(self) -> Option<metal_screen::ScreenFilter> {
        match self {
            PictureMode::Metal => Some(metal_screen::ScreenFilter::Nearest),
            PictureMode::Sharpen => Some(metal_screen::ScreenFilter::Sharpen),
            PictureMode::CpuNearest | PictureMode::Linear => None,
        }
    }
}

fn picture_modes() -> Vec<PictureMode> {
    let mut modes = Vec::new();
    #[cfg(target_os = "macos")]
    modes.push(PictureMode::Metal);
    #[cfg(target_os = "macos")]
    modes.push(PictureMode::Sharpen);
    modes.push(PictureMode::CpuNearest);
    modes.push(PictureMode::Linear);
    modes
}

fn picture_option_label(title: &str, open: bool) -> String {
    if open {
        format!("✓ {title}")
    } else {
        title.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PictureToggle {
    Focus,
    Close,
    Open,
    Switch,
}

fn picture_toggle(open_count: usize, already_open: bool) -> PictureToggle {
    if already_open {
        if open_count <= 1 {
            PictureToggle::Focus
        } else {
            PictureToggle::Close
        }
    } else if open_count == 1 {
        PictureToggle::Switch
    } else {
        PictureToggle::Open
    }
}

fn main_picture() -> PictureMode {
    #[cfg(target_os = "macos")]
    {
        PictureMode::Metal
    }
    #[cfg(not(target_os = "macos"))]
    {
        PictureMode::CpuNearest
    }
}

enum Command {
    Load(PathBuf),
    SetCover {
        rom_path: PathBuf,
        cover_path: PathBuf,
    },
    TogglePause,
    ToggleBiosHle,
    SaveStateSlot(u8),
    LoadStateSlot(u8),
    QuickSaveState,
    QuickLoadState,
    ResumeState,
    StartFresh,
    Reset,
    HostLink,
    JoinLink(String),
    StopLink,
    SetLinkName(String),
    Error(String),
    Shutdown,
}

impl Hotkey {
    fn command(self) -> Option<Command> {
        match self {
            Self::QuickSave => Some(Command::QuickSaveState),
            Self::QuickLoad => Some(Command::QuickLoadState),
            Self::FastForward => None,
        }
    }
}

struct ControlSettings {
    open: bool,
    capturing: Option<BindingTarget>,
    map: KeyMap,
    speed: u8,
    turbo: u8,
}

impl ControlSettings {
    fn saved(&self) -> SavedControls {
        SavedControls {
            map: self.map.clone(),
            speed: self.speed,
            turbo: self.turbo,
        }
    }
}

#[derive(Default)]
struct Snapshot {
    title: String,
    status: String,
    error: Option<String>,
    library_error: Option<String>,
    running: bool,
    bios_hle: bool,
    state_available: bool,
    resume_prompt: bool,
    boot_animation_started: Option<Instant>,
    rom_path: Option<PathBuf>,
    notice: String,
    frame_number: u64,
    image_version: u64,
    rgba: Vec<u8>,
    library: Vec<LibraryEntry>,
    library_version: u64,
    link_line: String,
    link_hosts: Vec<(String, String)>,
    link_error: Option<String>,
    link_lan: String,
    link_name: String,
    link_in_room: bool,
    link_seats: Vec<LinkSeat>,
}

enum LinkField {
    Address,
    Name,
}

struct LinkUi {
    open: bool,
    address: String,
    name: String,
    name_touched: bool,
    field: LinkField,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LibraryEntry {
    path: PathBuf,
    title: String,
    game_code: String,
    cover_path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct LibraryFile {
    version: u8,
    entries: Vec<LibraryEntry>,
}

struct ActiveCart {
    path: PathBuf,
    rom_id: String,
    title: String,
    game_code: String,
    cover_path: Option<PathBuf>,
    regs: Registers,
    bus: Bus,
}

struct AppView {
    snapshot: Arc<Mutex<Snapshot>>,
    commands: mpsc::Sender<Command>,
    keys: Arc<AtomicU16>,
    locked_speed: Arc<AtomicU8>,
    turbo_ratio: Arc<AtomicU8>,
    turbo_held: Arc<AtomicBool>,
    focus_handle: FocusHandle,
    image_version: u64,
    picture: PictureMode,
    compare: bool,
    labeled: bool,
    windows_alive: Arc<AtomicUsize>,
    picture_token: u64,
    picture_windows: Arc<Mutex<PictureWindows>>,
    chrome_dirty: Arc<AtomicBool>,
    #[cfg(target_os = "macos")]
    metal_screen: Option<Arc<Mutex<metal_screen::MetalScreen>>>,
    #[cfg(target_os = "macos")]
    viewport_sync_key: Option<(i32, i32)>,
    screen_rgba: Option<Vec<u8>>,
    screen_scale: u32,
    image: Option<Arc<RenderImage>>,
    library_version: u64,
    cover_images: HashMap<PathBuf, Option<Arc<RenderImage>>>,
    library_mode: Arc<AtomicBool>,
    controls: Arc<Mutex<ControlSettings>>,
    link_ui: Arc<Mutex<LinkUi>>,
}

impl AppView {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        snapshot: Arc<Mutex<Snapshot>>,
        commands: mpsc::Sender<Command>,
        keys: Arc<AtomicU16>,
        locked_speed: Arc<AtomicU8>,
        turbo_ratio: Arc<AtomicU8>,
        turbo_held: Arc<AtomicBool>,
        library_mode: Arc<AtomicBool>,
        controls: Arc<Mutex<ControlSettings>>,
        link_ui: Arc<Mutex<LinkUi>>,
        windows_alive: Arc<AtomicUsize>,
        picture_windows: Arc<Mutex<PictureWindows>>,
        chrome_dirty: Arc<AtomicBool>,
        picture_token: u64,
        picture: PictureMode,
        compare: bool,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        windows_alive.fetch_add(1, Ordering::AcqRel);
        #[cfg(target_os = "macos")]
        let metal_screen = if let Some(filter) = picture.metal_filter() {
            window.set_background_appearance(WindowBackgroundAppearance::Transparent);
            Some(Arc::new(Mutex::new(metal_screen::MetalScreen::new(filter))))
        } else {
            None
        };
        Self {
            snapshot,
            commands,
            keys,
            locked_speed,
            turbo_ratio,
            turbo_held,
            focus_handle,
            image_version: 0,
            picture,
            compare,
            labeled: compare,
            windows_alive,
            picture_token,
            picture_windows,
            chrome_dirty,
            #[cfg(target_os = "macos")]
            metal_screen,
            #[cfg(target_os = "macos")]
            viewport_sync_key: None,
            screen_rgba: None,
            screen_scale: 0,
            image: None,
            library_version: 0,
            cover_images: HashMap::new(),
            library_mode,
            controls,
            link_ui,
        }
    }

    fn select_library_rom(&mut self, path: PathBuf) {
        self.library_mode.store(false, Ordering::Relaxed);
        let _ = self.commands.send(Command::Load(path));
    }

    fn choose_rom(&self, cx: &mut Context<Self>) {
        prompt_rom_from_menu(cx, self.commands.clone(), Arc::clone(&self.library_mode));
    }

    fn controls_settings(&self) -> std::sync::MutexGuard<'_, ControlSettings> {
        self.controls.lock().expect("control settings poisoned")
    }

    fn release_turbo(&self) {
        self.turbo_held.store(false, Ordering::Relaxed);
    }

    fn begin_capture(&self, target: BindingTarget, cx: &mut Context<Self>) {
        self.controls_settings().capturing = Some(target);
        self.keys.store(0, Ordering::Relaxed);
        self.release_turbo();
        cx.notify();
    }

    fn close_controls(&self, cx: &mut Context<Self>) {
        let mut settings = self.controls_settings();
        settings.open = false;
        settings.capturing = None;
        drop(settings);
        self.keys.store(0, Ordering::Relaxed);
        self.release_turbo();
        cx.notify();
    }

    fn restore_default_controls(&self, cx: &mut Context<Self>) {
        let mut settings = self.controls_settings();
        settings.map = KeyMap::default();
        settings.speed = DEFAULT_SPEED;
        settings.turbo = DEFAULT_TURBO;
        settings.capturing = None;
        let saved = settings.saved();
        drop(settings);
        self.locked_speed.store(DEFAULT_SPEED, Ordering::Relaxed);
        self.turbo_ratio.store(DEFAULT_TURBO, Ordering::Relaxed);
        self.release_turbo();
        self.persist_controls(&saved);
        let picture_windows = Arc::clone(&self.picture_windows);
        let controls = Arc::clone(&self.controls);
        cx.defer(move |cx| {
            refresh_picture_chrome(cx, &picture_windows, &controls);
        });
        cx.notify();
    }

    fn persist_controls(&self, saved: &SavedControls) {
        let Ok(path) = controls_file_path() else {
            return;
        };
        if let Err(error) = write_controls(&path, saved) {
            let _ = self.commands.send(Command::Error(error));
        }
    }

    fn link_open(&self) -> bool {
        self.link_ui.lock().expect("link ui poisoned").open
    }

    fn close_link(&self, cx: &mut Context<Self>) {
        self.link_ui.lock().expect("link ui poisoned").open = false;
        self.keys.store(0, Ordering::Relaxed);
        self.release_turbo();
        cx.notify();
    }

    fn edit_link_field(&self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = normalize_key(&event.keystroke.key);
        if key == "escape" {
            self.close_link(cx);
            return;
        }
        let editing_name = matches!(
            self.link_ui.lock().expect("link ui poisoned").field,
            LinkField::Name
        );
        if key == "v" && event.keystroke.modifiers.platform && !event.is_held {
            if let Some(text) = clipboard_text() {
                if editing_name {
                    let keep_current = self
                        .link_ui
                        .lock()
                        .expect("link ui poisoned")
                        .name_touched;
                    self.push_link_name(text.chars(), keep_current, cx);
                } else {
                    let mut link = self.link_ui.lock().expect("link ui poisoned");
                    for ch in text.chars() {
                        if link.address.len() >= 64 {
                            break;
                        }
                        if ch.is_ascii_digit() || ch == '.' || ch == ':' {
                            link.address.push(ch);
                        }
                    }
                }
            }
            cx.notify();
            return;
        }
        if shortcut_chord(&event.keystroke.modifiers) {
            return;
        }
        if key == "enter" {
            if event.is_held || editing_name {
                return;
            }
            let address = self
                .link_ui
                .lock()
                .expect("link ui poisoned")
                .address
                .trim()
                .to_string();
            if !address.is_empty() {
                let _ = self.commands.send(Command::JoinLink(address));
            }
            cx.notify();
            return;
        }
        if key == "backspace" {
            if editing_name {
                let mut link = self.link_ui.lock().expect("link ui poisoned");
                self.touch_link_name(&mut link);
                if let Some((index, _)) = link.name.char_indices().next_back() {
                    link.name.truncate(index);
                }
                let name = link.name.clone();
                drop(link);
                self.commit_link_name(&name);
            } else {
                self.link_ui.lock().expect("link ui poisoned").address.pop();
            }
            cx.notify();
            return;
        }
        if event.is_held {
            return;
        }
        if editing_name {
            let Some(ch) = link_name_char(&key) else {
                return;
            };
            self.push_link_name(std::iter::once(ch), true, cx);
            cx.notify();
            return;
        }
        let Some(ch) = key.chars().next() else {
            return;
        };
        if key.chars().nth(1).is_none() && (ch.is_ascii_digit() || ch == '.' || ch == ':') {
            let mut link = self.link_ui.lock().expect("link ui poisoned");
            if link.address.len() < 64 {
                link.address.push(ch);
            }
            cx.notify();
        }
    }

    fn touch_link_name(&self, link: &mut LinkUi) {
        if link.name_touched {
            return;
        }
        link.name = self
            .snapshot
            .lock()
            .expect("emulator snapshot poisoned")
            .link_name
            .clone();
        link.name_touched = true;
    }

    fn push_link_name(
        &self,
        chars: impl Iterator<Item = char>,
        keep_current: bool,
        cx: &mut Context<Self>,
    ) {
        let mut link = self.link_ui.lock().expect("link ui poisoned");
        if keep_current {
            self.touch_link_name(&mut link);
        } else {
            link.name_touched = true;
            link.name.clear();
        }
        let mut next = link.name.clone();
        for ch in chars {
            if ch.is_control() {
                continue;
            }
            next.push(ch);
        }
        link.name = normalize_link_name(&next);
        let name = link.name.clone();
        drop(link);
        self.commit_link_name(&name);
        cx.notify();
    }

    fn commit_link_name(&self, name: &str) {
        persist_link_name(name);
        let _ = self.commands.send(Command::SetLinkName(name.to_string()));
    }

    fn handle_key_down(&self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.link_open() {
            self.edit_link_field(event, cx);
            return;
        }
        if shortcut_chord(&event.keystroke.modifiers) {
            return;
        }
        let key = normalize_key(&event.keystroke.key);
        let mut settings = self.controls_settings();
        if settings.open {
            if key == "escape" {
                if settings.capturing.take().is_some() {
                    drop(settings);
                    cx.notify();
                    return;
                }
                settings.open = false;
                drop(settings);
                self.keys.store(0, Ordering::Relaxed);
                self.release_turbo();
                cx.notify();
                return;
            }
            if let Some(target) = settings.capturing {
                if event.is_held || !bindable_key(&key) {
                    return;
                }
                settings.map.bind_target(target, &key);
                settings.capturing = None;
                let saved = settings.saved();
                drop(settings);
                self.persist_controls(&saved);
                cx.notify();
            }
            return;
        }
        let map = settings.map.clone();
        drop(settings);
        if is_reset_key(&key, &map) {
            if !event.is_held {
                let _ = self.commands.send(Command::Reset);
            }
            return;
        }
        if let Some(hotkey) = map.hotkey_for_key(&key) {
            if hotkey == Hotkey::FastForward {
                self.turbo_held.store(true, Ordering::Relaxed);
                return;
            }
            if !event.is_held {
                if let Some(command) = hotkey.command() {
                    let _ = self.commands.send(command);
                }
            }
            return;
        }
        self.set_key_bit(&key, &map, true);
    }

    fn handle_key_up(&self, event: &KeyUpEvent) {
        let settings = self.controls_settings();
        let open = settings.open;
        let map = settings.map.clone();
        drop(settings);
        if map.hotkey_for_key(&event.keystroke.key) == Some(Hotkey::FastForward) {
            self.release_turbo();
        }
        if open || self.link_open() {
            return;
        }
        self.set_key_bit(&event.keystroke.key, &map, false);
    }

    fn set_key_bit(&self, key: &str, map: &KeyMap, pressed: bool) {
        if game_key_mask(key, map).is_none() {
            return;
        }
        let _ = self
            .keys
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                Some(keyboard_bits(held, key, map, pressed))
            });
    }

    fn render_library(
        &self,
        cx: &mut Context<Self>,
        library: &[LibraryEntry],
        error: Option<String>,
    ) -> impl IntoElement {
        let cards = library.iter().rev().enumerate().map(|(index, entry)| {
            let path = entry.path.clone();
            let cover = entry
                .cover_path
                .as_ref()
                .and_then(|path| self.cover_images.get(path))
                .and_then(Clone::clone);
            let title = entry.title.clone();
            let code = entry.game_code.clone();
            div()
                .id(index)
                .w(px(176.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(10.))
                .rounded(px(9.))
                .bg(rgb(BG))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(0x202A2C)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_library_rom(path.clone());
                    cx.notify();
                }))
                .child(cover_art(cover, &title, &code, px(156.), px(208.)))
                .child(div().text_sm().text_color(rgb(TEXT)).child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(if code.is_empty() {
                            "GBA".to_string()
                        } else {
                            code
                        }),
                )
        });

        div()
            .flex()
            .flex_1()
            .flex_col()
            .gap(px(16.))
            .p(px(24.))
            .child(
                div()
                    .id("rom-import")
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .h(px(112.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(rgb(PANEL_HI))
                    .bg(rgb(0x101719))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x182123)))
                    .on_click(cx.listener(|this, _, _, cx| this.choose_rom(cx)))
                    .child(div().text_xl().text_color(rgb(ACCENT)).child("+"))
                    .child(div().text_sm().text_color(rgb(TEXT)).child("打开 GBA ROM"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("点击此区域选择 .gba 文件"),
                    ),
            )
            .children(error.map(|message| div().text_sm().text_color(rgb(0xE08B76)).child(message)))
            .child(
                div()
                    .pt(px(4.))
                    .text_lg()
                    .text_color(rgb(TEXT))
                    .child("最近使用的卡带"),
            )
            .child(
                div()
                    .id("rom-library")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(div().flex().flex_wrap().gap(px(12.)).children(cards)),
            )
    }

    fn adopt_picture(&mut self, mode: PictureMode, window: &Window) {
        if self.picture == mode {
            return;
        }
        let moved = {
            let mut windows = self
                .picture_windows
                .lock()
                .expect("picture windows poisoned");
            let slot = windows.slot_mut(self.picture).take();
            match slot {
                Some(slot) if slot.token == self.picture_token => {
                    windows.insert(mode, slot);
                    true
                }
                Some(slot) => {
                    windows.insert(self.picture, slot);
                    false
                }
                None => false,
            }
        };
        if !moved {
            return;
        }
        self.install_picture(mode, window);
        self.picture = mode;
        self.image_version = 0;
        self.screen_scale = 0;
        self.image = None;
        self.screen_rgba = None;
    }

    fn sync_stale_viewport(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        cached_w: f32,
        cached_h: f32,
        view_w: f32,
        view_h: f32,
    ) {
        #[cfg(not(target_os = "macos"))]
        let _ = (window, cx, cached_w, cached_h, view_w, view_h);
        #[cfg(target_os = "macos")]
        {
            let stale = (view_w - cached_w).abs() > 0.5 || (view_h - cached_h).abs() > 0.5;
            if !stale {
                self.viewport_sync_key = None;
                return;
            }
            let key = (view_w.round() as i32, view_h.round() as i32);
            if self.viewport_sync_key == Some(key) {
                return;
            }
            self.viewport_sync_key = Some(key);
            let Some(host) = native_view_ptr(window) else {
                return;
            };
            cx.defer(move |_| sync_native_view(host));
        }
    }

    fn install_picture(&mut self, mode: PictureMode, window: &Window) {
        #[cfg(target_os = "macos")]
        match mode.metal_filter() {
            Some(filter) => {
                window.set_background_appearance(WindowBackgroundAppearance::Transparent);
                if let Some(screen) = self.metal_screen.as_ref() {
                    screen
                        .lock()
                        .expect("metal screen poisoned")
                        .set_filter(filter);
                } else {
                    self.metal_screen = Some(Arc::new(Mutex::new(
                        metal_screen::MetalScreen::new(filter),
                    )));
                }
            }
            None => {
                window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                self.metal_screen = None;
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (mode, window);
    }

    fn paints_opaque_root(&self) -> bool {
        #[cfg(target_os = "macos")]
        if self.picture.metal_filter().is_some() {
            return self.library_mode.load(Ordering::Relaxed);
        }
        true
    }

    fn game_picture(&self, screen_w: f32, screen_h: f32, factor: u32) -> gpui::AnyElement {
        #[cfg(target_os = "macos")]
        if self.picture.metal_filter().is_some() {
            let presenter = Arc::clone(
                self.metal_screen
                    .as_ref()
                    .expect("metal screen"),
            );
            return canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    presenter
                        .lock()
                        .expect("metal screen poisoned")
                        .place(window, bounds, factor);
                },
            )
            .w(px(screen_w))
            .h(px(screen_h))
            .flex_shrink_0()
            .into_any_element();
        }
        let _ = factor;
        match &self.image {
            Some(image) => img(image.clone())
                .w(px(screen_w))
                .h(px(screen_h))
                .flex_shrink_0()
                .object_fit(gpui::ObjectFit::Fill)
                .into_any_element(),
            None => div()
                .w(px(screen_w))
                .h(px(screen_h))
                .flex_shrink_0()
                .bg(rgb(0x000000))
                .into_any_element(),
        }
    }
}

fn boot_progress(started_at: Option<Instant>, now: Instant) -> Option<f32> {
    let elapsed = now.checked_duration_since(started_at?)?;
    if elapsed >= boot_animation_duration() {
        None
    } else {
        Some((elapsed.as_secs_f32() / BOOT_ANIMATION_REVEAL_DURATION.as_secs_f32()).min(1.0))
    }
}

fn boot_animation_overlay(progress: f32) -> impl IntoElement {
    let progress = progress.clamp(0.0, 1.0);
    let landing = boot_ease_out((progress / 0.56).clamp(0.0, 1.0));
    let reveal = boot_ease_out(((progress - 0.52) / 0.18).clamp(0.0, 1.0));
    let glow = (1.0 - ((progress - 0.56) / 0.10).abs()).clamp(0.0, 1.0);
    let icon_color = mix_color(0x050809, ACCENT, landing);
    let wordmark_color = mix_color(0x050809, ACCENT, reveal);
    let tagline_color = mix_color(0x050809, MUTED, reveal);
    let emblem = div()
        .w(px(58.))
        .h(px(58.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.))
        .border_1()
        .border_color(rgb(icon_color))
        .child(
            div()
                .text_3xl()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(rgb(icon_color))
                .child("U"),
        );
    div()
        .absolute()
        .top(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .left(px(0.))
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x050809))
        .child(
            div()
                .relative()
                .mb(px(132. * (1.0 - landing)))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .child(emblem)
                .child(
                    div()
                        .w(px(140. + 70. * glow))
                        .h(px(2.))
                        .bg(rgb(mix_color(0x050809, ACCENT, glow))),
                )
                .child(
                    div()
                        .text_3xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(rgb(wordmark_color))
                        .child("ULTIMA"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(tagline_color))
                        .child("PORTABLE GAME SYSTEM"),
                ),
        )
}

fn boot_ease_out(progress: f32) -> f32 {
    let remaining = 1.0 - progress.clamp(0.0, 1.0);
    1.0 - remaining * remaining * remaining
}

fn mix_color(from: u32, to: u32, amount: f32) -> u32 {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |shift| {
        let start = ((from >> shift) & 0xFFu32) as f32;
        let end = ((to >> shift) & 0xFFu32) as f32;
        (start + (end - start) * amount).round() as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

impl Drop for AppView {
    fn drop(&mut self) {
        if self
            .picture_windows
            .lock()
            .expect("picture windows poisoned")
            .take_token(self.picture, self.picture_token)
        {
            self.chrome_dirty.store(true, Ordering::Release);
        }
        if self.windows_alive.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.commands.send(Command::Shutdown);
        }
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.chrome_dirty.swap(false, Ordering::AcqRel) {
            let picture_windows = Arc::clone(&self.picture_windows);
            let controls = Arc::clone(&self.controls);
            window.defer(cx, move |_, cx| {
                refresh_picture_chrome(cx, &picture_windows, &controls);
            });
        }
        let (
            error,
            image_version,
            rgba,
            library,
            library_version,
            resume_prompt,
            boot_animation_started,
            title,
            link_line,
            link_hosts,
            link_error,
            link_lan,
            link_name,
            link_in_room,
            link_seats,
        ) = {
            let snapshot = self.snapshot.lock().expect("emulator snapshot poisoned");
            (
                snapshot.error.clone(),
                snapshot.image_version,
                (snapshot.image_version != self.image_version).then(|| snapshot.rgba.clone()),
                snapshot.library.clone(),
                snapshot.library_version,
                snapshot.resume_prompt,
                snapshot.boot_animation_started,
                snapshot.title.clone(),
                snapshot.link_line.clone(),
                snapshot.link_hosts.clone(),
                snapshot.link_error.clone(),
                snapshot.link_lan.clone(),
                snapshot.link_name.clone(),
                snapshot.link_in_room,
                snapshot.link_seats.clone(),
            )
        };
        let (link_open, link_address, link_draft, link_touched, link_field) = {
            let link = self.link_ui.lock().expect("link ui poisoned");
            (
                link.open,
                link.address.clone(),
                link.name.clone(),
                link.name_touched,
                matches!(link.field, LinkField::Name),
            )
        };
        let shown_name = if link_touched {
            link_draft
        } else {
            link_name.clone()
        };
        let resume_error = resume_prompt.then(|| error.clone()).flatten();

        let viewport = window.viewport_size();
        let scale_factor = window.scale_factor();
        let cached_w = viewport.width / px(1.);
        let cached_h = viewport.height / px(1.);
        let frame = window.bounds().size;
        let (view_w, view_h) = resolve_picture_viewport(
            cached_w,
            cached_h,
            live_content_points(window),
            Some((frame.width / px(1.), frame.height / px(1.))),
        );
        self.sync_stale_viewport(window, cx, cached_w, cached_h, view_w, view_h);
        let screen_scale = integer_screen_scale(view_w, view_h, scale_factor);
        match self.picture {
            #[cfg(target_os = "macos")]
            PictureMode::Metal | PictureMode::Sharpen => {
                if let Some(rgba) = rgba {
                    self.image_version = image_version;
                    self.metal_screen
                        .as_ref()
                        .expect("metal screen")
                        .lock()
                        .expect("metal screen poisoned")
                        .upload(&rgba);
                }
            }
            PictureMode::CpuNearest => {
                let frame_updated = rgba.is_some();
                if let Some(rgba) = rgba {
                    self.screen_rgba = Some(rgba);
                    self.image_version = image_version;
                }
                if frame_updated || self.screen_scale != screen_scale {
                    if let Some(src) = self.screen_rgba.as_deref() {
                        if let Some(image) = scale_gba_nearest(src, screen_scale) {
                            self.image = Some(image);
                            self.screen_scale = screen_scale;
                        }
                    }
                }
            }
            PictureMode::Linear => {
                if let Some(rgba) = rgba {
                    self.image_version = image_version;
                    if let Some(image) = scale_gba_nearest(&rgba, 1) {
                        self.image = Some(image);
                    }
                }
            }
        }
        let (screen_w, screen_h) = screen_point_size(screen_scale, scale_factor);
        if self.library_version != library_version {
            self.cover_images.clear();
            for entry in &library {
                if let Some(path) = &entry.cover_path {
                    self.cover_images
                        .insert(path.clone(), load_cover_image(path).ok());
                }
            }
            self.library_version = library_version;
        }

        window.request_animation_frame();

        let (controls_open, controls_capturing, controls_map) = {
            let settings = self.controls_settings();
            (settings.open, settings.capturing, settings.map.clone())
        };
        let speed_now = effective_speed(
            self.locked_speed.load(Ordering::Relaxed),
            self.turbo_ratio.load(Ordering::Relaxed),
            self.turbo_held.load(Ordering::Relaxed),
        );

        let screen = self.game_picture(screen_w, screen_h, screen_scale);
        let content = if !self.compare && self.library_mode.load(Ordering::Relaxed) {
            self.render_library(cx, &library, error).into_any_element()
        } else {
            let game = div()
                .size_full()
                .relative()
                .child(letterboxed_screen(screen, screen_h))
                .when(speed_now > 1, |game| game.child(speed_badge(speed_now)))
                .when(link_in_room, |game| game.child(link_board(&link_seats)))
                .when(!link_in_room && !link_line.is_empty(), |game| {
                    game.child(link_chip(&link_line))
                })
                .when_some(error, |content, message| {
                    content.child(
                        div()
                            .absolute()
                            .top(px(12.))
                            .left(px(12.))
                            .max_w(px(520.))
                            .p(px(10.))
                            .rounded(px(6.))
                            .bg(rgb(0x301E1B))
                            .text_color(rgb(0xE08B76))
                            .child(message),
                    )
                });
            let game = if !self.compare && resume_prompt {
                game.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .right(px(0.))
                        .bottom(px(0.))
                        .left(px(0.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(BG))
                        .child(
                            div()
                                .w(px(340.))
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(14.))
                                .p(px(22.))
                                .rounded(px(10.))
                                .bg(rgb(PANEL_HI))
                                .child(
                                    div()
                                        .text_lg()
                                        .text_color(rgb(TEXT))
                                        .child("发现上次的游戏进度"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(MUTED))
                                        .child(format!("{} 有可继续的即时存档", title)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(10.))
                                        .child(
                                            div()
                                                .id("resume-last-session")
                                                .px(px(14.))
                                                .py(px(8.))
                                                .rounded(px(6.))
                                                .bg(rgb(ACCENT))
                                                .text_color(rgb(BG))
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, _| {
                                                    let _ =
                                                        this.commands.send(Command::ResumeState);
                                                }))
                                                .child("继续上次"),
                                        )
                                        .child(
                                            div()
                                                .id("start-from-beginning")
                                                .px(px(14.))
                                                .py(px(8.))
                                                .rounded(px(6.))
                                                .bg(rgb(BG))
                                                .text_color(rgb(TEXT))
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, _| {
                                                    let _ = this.commands.send(Command::StartFresh);
                                                }))
                                                .child("从头开始"),
                                        ),
                                )
                                .children(resume_error.map(|message| {
                                    div()
                                        .max_w(px(300.))
                                        .text_xs()
                                        .text_color(rgb(0xE08B76))
                                        .child(message)
                                })),
                        ),
                )
            } else {
                game
            };
            let game = if self.compare {
                game
            } else {
                game.when_some(
                    boot_progress(boot_animation_started, Instant::now()),
                    |game, progress| game.child(boot_animation_overlay(progress)),
                )
            };
            game.when(self.labeled, |game| game.child(picture_label(self.picture)))
                .into_any_element()
        };

        div()
            .id("app")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.handle_key_down(event, cx);
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                this.handle_key_up(event);
            }))
            .size_full()
            .flex()
            .flex_col()
            .when(self.paints_opaque_root(), |app| app.bg(rgb(0x000000)))
            .child(content)
            .when(!self.compare && controls_open, |app| {
                app.child(controls_overlay(cx, &controls_map, controls_capturing))
            })
            .when(!self.compare && !controls_open && link_open, |app| {
                app.child(link_overlay(
                    cx,
                    &link_address,
                    &shown_name,
                    link_field,
                    &link_line,
                    link_error.as_deref(),
                    &link_lan,
                    &link_hosts,
                    link_in_room,
                    &link_seats,
                ))
            })
    }
}

fn cover_art(
    image: Option<Arc<RenderImage>>,
    title: &str,
    game_code: &str,
    width: gpui::Pixels,
    height: gpui::Pixels,
) -> impl IntoElement {
    if let Some(image) = image {
        img(image).w(width).h(height).into_any_element()
    } else {
        div()
            .w(width)
            .h(height)
            .flex()
            .flex_col()
            .justify_between()
            .p(px(12.))
            .rounded(px(7.))
            .bg(rgb(PANEL_HI))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(ACCENT))
                    .child("GAME BOY ADVANCE"),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if game_code.is_empty() {
                        "NO COVER".to_string()
                    } else {
                        game_code.to_owned()
                    }),
            )
            .into_any_element()
    }
}

fn picture_label(mode: PictureMode) -> impl IntoElement {
    div()
        .absolute()
        .top(px(10.))
        .left(px(10.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(4.))
        .bg(rgb(0x101417))
        .text_xs()
        .text_color(rgb(ACCENT))
        .child(mode.title())
}

fn letterboxed_screen(screen: impl gpui::IntoElement, screen_h: f32) -> gpui::AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(div().flex_1().bg(rgb(0x000000)))
        .child(
            div()
                .h(px(screen_h))
                .w_full()
                .flex_shrink_0()
                .flex()
                .flex_row()
                .child(div().flex_1().h_full().bg(rgb(0x000000)))
                .child(screen)
                .child(div().flex_1().h_full().bg(rgb(0x000000))),
        )
        .child(div().flex_1().bg(rgb(0x000000)))
        .into_any_element()
}

/// Prefer the live content size. GPUI keeps `viewport_size` from the last
/// resize callback, and macOS fullscreen can leave that callback unfired
/// while the window frame has already grown.
fn resolve_picture_viewport(
    cached_w: f32,
    cached_h: f32,
    live_content: Option<(f32, f32)>,
    window_frame: Option<(f32, f32)>,
) -> (f32, f32) {
    let mut width = cached_w;
    let mut height = cached_h;
    if let Some((live_w, live_h)) = live_content {
        if live_w.is_finite() && live_h.is_finite() && live_w > 1.0 && live_h > 1.0 {
            width = live_w;
            height = live_h;
        }
    }
    if let Some((frame_w, frame_h)) = window_frame {
        // The frame is taller than the content by about a title bar.
        // A fullscreen or zoomed window is hundreds of points larger.
        if frame_w.is_finite() && frame_w > width + 80.0 {
            width = frame_w;
        }
        if frame_h.is_finite() && frame_h > height + 80.0 {
            height = frame_h;
        }
    }
    (width, height)
}

#[cfg(target_os = "macos")]
fn live_content_points(window: &Window) -> Option<(f32, f32)> {
    use cocoa::appkit::NSView;
    use cocoa::base::{id, nil};
    use objc::{msg_send, sel, sel_impl};

    let host = native_view_ptr(window)?;
    unsafe {
        let ns_window: id = msg_send![host, window];
        if ns_window == nil {
            return None;
        }
        let content: id = msg_send![ns_window, contentView];
        if content == nil {
            return None;
        }
        let frame = NSView::frame(content);
        let width = frame.size.width as f32;
        let height = frame.size.height as f32;
        (width.is_finite() && height.is_finite() && width > 1.0 && height > 1.0)
            .then_some((width, height))
    }
}

#[cfg(not(target_os = "macos"))]
fn live_content_points(_window: &Window) -> Option<(f32, f32)> {
    None
}

#[cfg(target_os = "macos")]
fn native_view_ptr(window: &Window) -> Option<cocoa::base::id> {
    use cocoa::base::id;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return None;
    };
    match handle.as_raw() {
        RawWindowHandle::AppKit(appkit) => Some(appkit.ns_view.as_ptr() as id),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn sync_native_view(host: cocoa::base::id) {
    use cocoa::appkit::{NSView, NSWindow};
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSPoint, NSRect, NSSize};
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        if host == nil {
            return;
        }
        let ns_window: id = msg_send![host, window];
        if ns_window == nil {
            return;
        }
        let content: id = msg_send![ns_window, contentView];
        if content == nil {
            return;
        }
        let window_frame = NSWindow::frame(ns_window);
        let content_rect: NSRect = msg_send![ns_window, contentRectForFrameRect: window_frame];
        let content_frame = NSView::frame(content);
        if (content_frame.size.width - content_rect.size.width).abs() > 80.0
            || (content_frame.size.height - content_rect.size.height).abs() > 80.0
        {
            let filled = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(content_rect.size.width, content_rect.size.height),
            );
            let _: () = msg_send![content, setFrame: filled];
        }
        let mut target = NSView::bounds(content);
        target.origin.x = 0.0;
        target.origin.y = 0.0;
        let host_frame = NSView::frame(host);
        let same = (host_frame.size.width - target.size.width).abs() < 0.5
            && (host_frame.size.height - target.size.height).abs() < 0.5;
        if same {
            // GPUI's setFrameSize returns before telling the window when the
            // size is unchanged, so a fullscreen content view can leave the
            // cached viewport at the windowed size.
            let mut smaller = target;
            smaller.size.width = (smaller.size.width - 1.0).max(1.0);
            let _: () = msg_send![host, setFrame: smaller];
        }
        let _: () = msg_send![host, setFrame: target];
    }
}

fn integer_screen_scale(viewport_w: f32, viewport_h: f32, scale_factor: f32) -> u32 {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let device_w = (viewport_w * scale_factor).floor().max(0.0);
    let device_h = (viewport_h * scale_factor).floor().max(0.0);
    let fit_w = (device_w / WIDTH as f32).floor() as u32;
    let fit_h = (device_h / HEIGHT as f32).floor() as u32;
    fit_w.min(fit_h).max(1)
}

fn screen_point_size(factor: u32, scale_factor: f32) -> (f32, f32) {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    (
        WIDTH as f32 * factor as f32 / scale_factor,
        HEIGHT as f32 * factor as f32 / scale_factor,
    )
}

fn scale_gba_nearest_rgba(src: &[u8], factor: u32) -> Option<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    if factor == 0 || src.len() != WIDTH * HEIGHT * 4 {
        return None;
    }
    let factor = factor as usize;
    let dw = WIDTH * factor;
    let dh = HEIGHT * factor;
    let mut dst = vec![0u8; dw * dh * 4];
    for y in 0..HEIGHT {
        let src_row = &src[y * WIDTH * 4..(y + 1) * WIDTH * 4];
        let mut row = vec![0u8; dw * 4];
        for x in 0..WIDTH {
            let pixel = &src_row[x * 4..x * 4 + 4];
            for step in 0..factor {
                let offset = (x * factor + step) * 4;
                row[offset..offset + 4].copy_from_slice(pixel);
            }
        }
        for step in 0..factor {
            let start = (y * factor + step) * dw * 4;
            dst[start..start + row.len()].copy_from_slice(&row);
        }
    }
    ImageBuffer::from_raw(dw as u32, dh as u32, dst)
}

fn scale_gba_nearest(src: &[u8], factor: u32) -> Option<Arc<RenderImage>> {
    let buffer = rgba_buffer_to_bgra(scale_gba_nearest_rgba(src, factor)?);
    Some(Arc::new(RenderImage::new(vec![ImageFrame::new(buffer)])))
}

fn load_cover_image(path: &Path) -> image::ImageResult<Arc<RenderImage>> {
    let cover = image::open(path)?
        .resize_to_fill(240, 320, image::imageops::FilterType::Triangle)
        .into_rgba8();
    let cover = rgba_buffer_to_bgra(cover);
    Ok(Arc::new(RenderImage::new(vec![ImageFrame::new(cover)])))
}

fn rgba_buffer_to_bgra(
    mut image: ImageBuffer<Rgba<u8>, Vec<u8>>,
) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    image
}

fn shortcut_chord(modifiers: &gpui::Modifiers) -> bool {
    modifiers.platform || modifiers.control || modifiers.alt
}

fn clipboard_text() -> Option<String> {
    let output = std::process::Command::new("pbpaste").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn controls_file_path() -> Result<PathBuf, String> {
    Ok(application_data_dir()?.join("controls.json"))
}

fn link_name_path() -> Result<PathBuf, String> {
    Ok(application_data_dir()?.join("link-name.txt"))
}

fn load_link_name() -> String {
    link_name_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| normalize_link_name(&text))
        .unwrap_or_default()
}

fn persist_link_name(name: &str) {
    let Ok(path) = link_name_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, name);
}

fn load_saved_controls() -> SavedControls {
    match controls_file_path().and_then(|path| read_controls(&path)) {
        Ok(saved) => saved,
        Err(error) => {
            eprintln!("{error}");
            SavedControls::default()
        }
    }
}

const KEYMAP_WIDTH: f32 = 560.;
const KEYMAP_HEIGHT: f32 = 271.;
const KEY_CLUSTER_WIDTH: f32 = 184.;
const KEYMAP_GAP: f32 = 12.;
const KEYMAP_ROW_WIDTH: f32 =
    KEY_CLUSTER_WIDTH + KEYMAP_GAP + KEYMAP_WIDTH + KEYMAP_GAP + KEY_CLUSTER_WIDTH;
const KEY_FIELD_WIDTH: f32 = 56.;
const KEY_FIELD_HEIGHT: f32 = 22.;

fn keymap_png_bytes() -> &'static [u8] {
    include_bytes!("../../assets/keymap.png")
}

fn keymap_image() -> Arc<RenderImage> {
    static IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            let decoded = image::load_from_memory(keymap_png_bytes())
                .expect("decode assets/keymap.png")
                .into_rgba8();
            let decoded = rgba_buffer_to_bgra(decoded);
            Arc::new(RenderImage::new(vec![ImageFrame::new(decoded)]))
        })
        .clone()
}

fn controls_overlay(
    cx: &mut Context<AppView>,
    map: &KeyMap,
    capturing: Option<BindingTarget>,
) -> impl IntoElement {
    let hint = match capturing {
        Some(target) => format!("按下要绑定到 {} 的键", target.name()),
        None => "点输入框，再按键盘。Esc 关闭。手柄走默认键位，和键盘同时有效。".into(),
    };
    div()
        .absolute()
        .top(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .left(px(0.))
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x050809))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.))
                .p(px(18.))
                .rounded(px(16.))
                .bg(rgb(PANEL_HI))
                .border_1()
                .border_color(rgb(0x314044))
                .child(div().text_lg().text_color(rgb(TEXT)).child("键位"))
                .child(div().text_sm().text_color(rgb(MUTED)).child(hint))
                .child(console_keymap(cx, map, capturing))
                .child(reset_binding(cx, map, capturing))
                .child(function_bindings(cx, map, capturing))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("复位会重新启动当前卡带，游戏存档会保留。快速读档载入最近一份即时存档。"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("机身照片 Joonas Javanainen · CC BY 4.0"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            div()
                                .id("controls-defaults")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(BG))
                                .text_color(rgb(TEXT))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.restore_default_controls(cx);
                                }))
                                .child("恢复默认"),
                        )
                        .child(
                            div()
                                .id("controls-done")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(ACCENT))
                                .text_color(rgb(BG))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_controls(cx);
                                }))
                                .child("完成"),
                        ),
                ),
        )
}

fn console_keymap(
    cx: &mut Context<AppView>,
    map: &KeyMap,
    capturing: Option<BindingTarget>,
) -> impl IntoElement {
    div()
        .w(px(KEYMAP_ROW_WIDTH))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .w_full()
                .flex()
                .items_end()
                .justify_between()
                .child(labeled_field(cx, map, capturing, PadButton::L))
                .child(
                    div()
                        .flex()
                        .gap(px(16.))
                        .child(labeled_field(cx, map, capturing, PadButton::Select))
                        .child(labeled_field(cx, map, capturing, PadButton::Start)),
                )
                .child(labeled_field(cx, map, capturing, PadButton::R)),
        )
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(dpad_fields(cx, map, capturing))
                .child(
                    img(keymap_image())
                        .w(px(KEYMAP_WIDTH))
                        .h(px(KEYMAP_HEIGHT))
                        .object_fit(gpui::ObjectFit::Fill),
                )
                .child(ab_fields(cx, map, capturing)),
        )
}

fn dpad_fields(
    cx: &mut Context<AppView>,
    map: &KeyMap,
    capturing: Option<BindingTarget>,
) -> impl IntoElement {
    div()
        .w(px(KEY_CLUSTER_WIDTH))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .child(labeled_field(cx, map, capturing, PadButton::Up))
        .child(
            div()
                .w_full()
                .flex()
                .justify_between()
                .child(labeled_field(cx, map, capturing, PadButton::Left))
                .child(labeled_field(cx, map, capturing, PadButton::Right)),
        )
        .child(labeled_field(cx, map, capturing, PadButton::Down))
}

fn ab_fields(
    cx: &mut Context<AppView>,
    map: &KeyMap,
    capturing: Option<BindingTarget>,
) -> impl IntoElement {
    div()
        .w(px(KEY_CLUSTER_WIDTH))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .justify_end()
                .child(labeled_field(cx, map, capturing, PadButton::A)),
        )
        .child(
            div()
                .flex()
                .justify_start()
                .child(labeled_field(cx, map, capturing, PadButton::B)),
        )
}

fn labeled_field(
    cx: &mut Context<AppView>,
    map: &KeyMap,
    capturing: Option<BindingTarget>,
    button: PadButton,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(3.))
        .child(div().text_xs().text_color(rgb(MUTED)).child(button.name()))
        .child(key_field(
            cx,
            BindingTarget::Pad(button),
            map.key(button),
            capturing == Some(BindingTarget::Pad(button)),
        ))
}

fn reset_binding(
    cx: &mut Context<AppView>,
    map: &KeyMap,
    capturing: Option<BindingTarget>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .text_sm()
                .text_color(rgb(TEXT))
                .child(PadButton::Reset.name()),
        )
        .child(key_field(
            cx,
            BindingTarget::Pad(PadButton::Reset),
            map.key(PadButton::Reset),
            capturing == Some(BindingTarget::Pad(PadButton::Reset)),
        ))
}

fn function_bindings(
    cx: &mut Context<AppView>,
    map: &KeyMap,
    capturing: Option<BindingTarget>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(22.))
        .child(named_hotkey(cx, map, capturing, Hotkey::QuickSave))
        .child(named_hotkey(cx, map, capturing, Hotkey::QuickLoad))
        .child(named_hotkey(cx, map, capturing, Hotkey::FastForward))
}

fn named_hotkey(
    cx: &mut Context<AppView>,
    map: &KeyMap,
    capturing: Option<BindingTarget>,
    hotkey: Hotkey,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().text_sm().text_color(rgb(TEXT)).child(hotkey.name()))
        .child(key_field(
            cx,
            BindingTarget::Hotkey(hotkey),
            map.hotkey(hotkey),
            capturing == Some(BindingTarget::Hotkey(hotkey)),
        ))
}

fn key_field(
    cx: &mut Context<AppView>,
    target: BindingTarget,
    bound: &str,
    capturing: bool,
) -> impl IntoElement {
    let label = if capturing {
        "请按键".to_string()
    } else {
        key_label(bound)
    };
    div()
        .id(target.element_id())
        .w(px(KEY_FIELD_WIDTH))
        .h(px(KEY_FIELD_HEIGHT))
        .rounded(px(3.))
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(rgb(if capturing { ACCENT } else { 0xC9C3B6 }))
        .bg(rgb(if capturing { 0xFFFDF6 } else { 0xF4F1EA }))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(if capturing { 0xFFFDF6 } else { 0xE8E3D8 })))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.begin_capture(target, cx);
        }))
        .child(
            div()
                .text_xs()
                .whitespace_nowrap()
                .text_color(rgb(0x1C1915))
                .child(label),
        )
}

fn link_chip(line: &str) -> impl IntoElement {
    div()
        .absolute()
        .bottom(px(12.))
        .left(px(12.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(4.))
        .bg(rgb(PANEL_HI))
        .text_color(rgb(ACCENT))
        .text_sm()
        .child(line.to_string())
}

fn link_board(seats: &[LinkSeat]) -> impl IntoElement {
    let online = seats.iter().filter(|seat| !seat.name.is_empty()).count();
    div()
        .absolute()
        .bottom(px(12.))
        .left(px(12.))
        .w(px(188.))
        .flex()
        .flex_col()
        .gap(px(3.))
        .p(px(8.))
        .rounded(px(6.))
        .bg(rgb(PANEL_HI))
        .child(
            div()
                .flex()
                .justify_between()
                .text_xs()
                .text_color(rgb(ACCENT))
                .child("联机")
                .child(format!("在线 {online}/4")),
        )
        .children(seats.iter().map(|seat| {
            let open = seat.name.is_empty();
            div()
                .flex()
                .justify_between()
                .text_xs()
                .text_color(rgb(if open { MUTED } else { TEXT }))
                .child(format!(
                    "{}  {}",
                    seat.number,
                    if open {
                        "空位".to_string()
                    } else {
                        short_link_name(&seat.name)
                    }
                ))
                .child(if seat.mine {
                    "你".to_string()
                } else if open {
                    String::new()
                } else {
                    "在线".to_string()
                })
        }))
}

fn link_overlay(
    cx: &mut Context<AppView>,
    address: &str,
    name: &str,
    editing_name: bool,
    line: &str,
    error: Option<&str>,
    lan_ip: &str,
    hosts: &[(String, String)],
    in_room: bool,
    seats: &[LinkSeat],
) -> impl IntoElement {
    let status = if line.is_empty() {
        "未连接".to_string()
    } else {
        line.to_string()
    };
    let address_text = if address.is_empty() {
        "输入 IP，或直接点下面的主机".to_string()
    } else {
        address.to_string()
    };
    let name_text = if name.is_empty() {
        "点这里，留空就用电脑名".to_string()
    } else {
        name.to_string()
    };
    let room = if in_room && seats.len() == 4 {
        seats.to_vec()
    } else {
        (1..=4)
            .map(|number| LinkSeat {
                number,
                name: String::new(),
                mine: false,
            })
            .collect()
    };
    let online = room.iter().filter(|seat| !seat.name.is_empty()).count();
    let room_label = if in_room {
        format!("房间 · 在线 {online}/4")
    } else {
        "房间 · 还没进入".to_string()
    };
    let found = if hosts.is_empty() {
        div()
            .text_sm()
            .text_color(rgb(MUTED))
            .child("同一 Wi-Fi 下，对方选「做主机」后会出现在这里。")
            .into_any_element()
    } else {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(hosts.iter().enumerate().map(|(index, (name, addr))| {
                let addr = addr.clone();
                let label = format!("{name}  ·  {addr}");
                div()
                    .id(("link-found", index))
                    .w_full()
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .bg(rgb(BG))
                    .text_color(rgb(TEXT))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x314044)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let _ = this.commands.send(Command::JoinLink(addr.clone()));
                        cx.notify();
                    }))
                    .child(label)
            }))
            .into_any_element()
    };
    div()
        .absolute()
        .top(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .left(px(0.))
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x050809))
        .child(
            div()
                .id("link-panel")
                .w(px(440.))
                .max_h(px(640.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(18.))
                .rounded(px(16.))
                .bg(rgb(PANEL_HI))
                .border_1()
                .border_color(rgb(0x314044))
                .child(div().text_lg().text_color(rgb(TEXT)).child("联机"))
                .child(div().text_sm().text_color(rgb(ACCENT)).child(status))
                .children(error.filter(|message| *message != line).map(|message| {
                    div()
                        .text_sm()
                        .text_color(rgb(0xE08B76))
                        .child(message.to_string())
                }))
                .child(div().text_sm().text_color(rgb(TEXT)).child("你的名字"))
                .child(
                    div()
                        .id("link-name")
                        .w_full()
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .bg(rgb(BG))
                        .border_1()
                        .border_color(rgb(if editing_name { ACCENT } else { 0x314044 }))
                        .text_color(rgb(if name.is_empty() { MUTED } else { TEXT }))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.link_ui.lock().expect("link ui poisoned").field = LinkField::Name;
                            cx.notify();
                        }))
                        .child(name_text),
                )
                .child(div().text_sm().text_color(rgb(TEXT)).child(room_label))
                .children(room.iter().map(|seat| link_seat_row(seat)))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            div()
                                .id("link-host")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(ACCENT))
                                .text_color(rgb(BG))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let _ = this.commands.send(Command::HostLink);
                                    cx.notify();
                                }))
                                .child("做主机"),
                        )
                        .child(
                            div()
                                .id("link-stop")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(BG))
                                .text_color(rgb(TEXT))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let _ = this.commands.send(Command::StopLink);
                                    cx.notify();
                                }))
                                .child("断开"),
                        ),
                )
                .child(div().text_sm().text_color(rgb(TEXT)).child("附近的主机"))
                .child(found)
                .child(div().text_sm().text_color(rgb(TEXT)).child("手动加入"))
                .child(
                    div()
                        .id("link-address")
                        .w_full()
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .bg(rgb(BG))
                        .border_1()
                        .border_color(rgb(if editing_name { 0x314044 } else { ACCENT }))
                        .text_color(if address.is_empty() {
                            rgb(MUTED)
                        } else {
                            rgb(TEXT)
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.link_ui.lock().expect("link ui poisoned").field = LinkField::Address;
                            cx.notify();
                        }))
                        .child(address_text),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            div()
                                .id("link-join")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(ACCENT))
                                .text_color(rgb(BG))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let address = this
                                        .link_ui
                                        .lock()
                                        .expect("link ui poisoned")
                                        .address
                                        .trim()
                                        .to_string();
                                    if !address.is_empty() {
                                        let _ = this.commands.send(Command::JoinLink(address));
                                    }
                                    cx.notify();
                                }))
                                .child("加入"),
                        )
                        .child(
                            div()
                                .id("link-done")
                                .px(px(14.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(rgb(BG))
                                .text_color(rgb(TEXT))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_link(cx);
                                }))
                                .child("完成"),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(format!(
                            "本机 {lan_ip}:5576。点名字或地址再输入，Command-V 粘贴。同一张卡带，主机是 1 号机。Esc 只关闭面板。"
                        )),
                ),
        )
}

fn link_seat_row(seat: &LinkSeat) -> impl IntoElement {
    let open = seat.name.is_empty();
    div()
        .w_full()
        .flex()
        .justify_between()
        .items_center()
        .px(px(12.))
        .py(px(6.))
        .rounded(px(6.))
        .bg(rgb(BG))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_sm()
                .text_color(rgb(if open { MUTED } else { TEXT }))
                .child(format!("{}号机", seat.number))
                .child(if open {
                    "空位".to_string()
                } else {
                    seat.name.clone()
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(if seat.mine { ACCENT } else { MUTED }))
                .child(if seat.mine {
                    "你".to_string()
                } else if open {
                    String::new()
                } else {
                    "在线".to_string()
                }),
        )
}

fn short_link_name(name: &str) -> String {
    let mut short = String::new();
    for (index, ch) in name.chars().enumerate() {
        if index == 10 {
            short.push('…');
            break;
        }
        short.push(ch);
    }
    short
}

fn link_name_char(key: &str) -> Option<char> {
    match key {
        "space" => Some(' '),
        "minus" => Some('-'),
        other if other.chars().count() == 1 => {
            let ch = other.chars().next()?;
            (!ch.is_control()).then_some(ch)
        }
        _ => None,
    }
}

fn speed_badge(speed: u8) -> impl IntoElement {
    div()
        .absolute()
        .top(px(12.))
        .right(px(12.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(4.))
        .bg(rgb(PANEL_HI))
        .text_color(rgb(ACCENT))
        .text_sm()
        .child(format!("{speed}×"))
}

fn start_emulator(
    locked_speed: Arc<AtomicU8>,
    turbo_ratio: Arc<AtomicU8>,
    turbo_held: Arc<AtomicBool>,
) -> (
    Arc<Mutex<Snapshot>>,
    mpsc::Sender<Command>,
    Arc<AtomicU16>,
    Arc<AtomicU8>,
    thread::JoinHandle<()>,
) {
    let (library, library_path, library_error) = match library_file_path() {
        Ok(path) => match read_library(&path) {
            Ok(library) => (library, Some(path), None),
            Err(error) => (Vec::new(), None, Some(error)),
        },
        Err(error) => (Vec::new(), None, Some(error)),
    };
    let (commands, receiver) = mpsc::channel();
    let snapshot = Arc::new(Mutex::new(Snapshot {
        status: "READY".into(),
        bios_hle: true,
        error: library_error.clone(),
        library_error,
        library,
        library_version: 1,
        ..Snapshot::default()
    }));
    let keys = Arc::new(AtomicU16::new(0));
    let audio_volume = Arc::new(AtomicU8::new(70));
    let worker_snapshot = Arc::clone(&snapshot);
    let worker_keys = Arc::clone(&keys);
    let worker_audio_volume = Arc::clone(&audio_volume);
    let worker_library_path = library_path;
    let worker_locked_speed = Arc::clone(&locked_speed);
    let worker_turbo_ratio = Arc::clone(&turbo_ratio);
    let worker_turbo_held = Arc::clone(&turbo_held);
    let worker = thread::Builder::new()
        .name("ultima-emulator".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            emulator_worker(
                receiver,
                worker_snapshot,
                worker_keys,
                worker_audio_volume,
                worker_library_path,
                worker_locked_speed,
                worker_turbo_ratio,
                worker_turbo_held,
            )
        })
        .expect("start emulator worker");
    (snapshot, commands, keys, audio_volume, worker)
}

fn drive_link(link: &mut LinkHub, cart: &mut Option<ActiveCart>, snapshot: &Mutex<Snapshot>) {
    if let Some(active) = cart.as_mut() {
        link.service(&mut active.bus);
    } else {
        link.poll();
    }
    publish_link(snapshot, link);
}

fn publish_link(snapshot: &Mutex<Snapshot>, link: &LinkHub) {
    let view = link.view();
    let mut state = snapshot.lock().expect("emulator snapshot poisoned");
    write_link_view(&mut state, &view);
}

fn write_link_view(state: &mut Snapshot, view: &gameboy_ultima::link::LinkView) {
    state.link_line = view.line.clone();
    state.link_hosts = view.hosts.clone();
    state.link_error = view.error.clone();
    state.link_lan = view.lan_ip.clone();
    state.link_name = view.self_name.clone();
    state.link_in_room = view.in_room;
    state.link_seats = view.seats.clone();
}

fn take_link_command(
    command: Command,
    link: &mut LinkHub,
    cart: &mut Option<ActiveCart>,
    snapshot: &Mutex<Snapshot>,
) -> Option<Command> {
    let replaces_cart = matches!(
        command,
        Command::Load(_)
            | Command::Reset
            | Command::StartFresh
            | Command::LoadStateSlot(_)
            | Command::QuickLoadState
            | Command::ResumeState
    );
    if replaces_cart {
        link.stop();
        if let Some(active) = cart.as_mut() {
            active.bus.set_link_player(None);
        }
    }
    match command {
        Command::HostLink => {
            match cart.as_ref() {
                Some(active) => {
                    let rom_id = active.rom_id.clone();
                    let _ = link.host(&rom_id);
                }
                None => link.note("先打开一张卡带，再做主机"),
            }
            publish_link(snapshot, link);
            None
        }
        Command::JoinLink(address) => {
            match cart.as_ref() {
                Some(active) => {
                    let rom_id = active.rom_id.clone();
                    let _ = link.join(&address, &rom_id);
                }
                None => link.note("先打开同一张卡带，再加入"),
            }
            publish_link(snapshot, link);
            None
        }
        Command::StopLink => {
            link.stop();
            if let Some(active) = cart.as_mut() {
                active.bus.set_link_player(None);
            }
            publish_link(snapshot, link);
            None
        }
        Command::SetLinkName(name) => {
            link.set_name(&name);
            publish_link(snapshot, link);
            None
        }
        other => Some(other),
    }
}

fn emulator_worker(
    receiver: mpsc::Receiver<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    keys: Arc<AtomicU16>,
    audio_volume: Arc<AtomicU8>,
    library_path: Option<PathBuf>,
    locked_speed: Arc<AtomicU8>,
    turbo_ratio: Arc<AtomicU8>,
    turbo_held: Arc<AtomicBool>,
) {
    let mut cart: Option<ActiveCart> = None;
    let mut audio: Option<AudioPlay> = None;
    let mut paused = false;
    let mut bios_hle = true;
    let mut frame_number = 0u64;
    let mut screenshot_frames_left: Option<u32> = None;
    let frame_interval = Duration::from_secs_f64(CYCLES_PER_FRAME as f64 / 16_777_216.0);
    let mut next_frame_deadline = Instant::now();
    let mut pads = open_gamepads();
    let mut link = LinkHub::new();
    let mut link_steps = 0u32;

    'worker: loop {
        let pad = poll_gamepad(&mut pads);
        if cart.is_none() || paused {
            drive_link(&mut link, &mut cart, &snapshot);
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => {
                    if let Some(command) =
                        take_link_command(command, &mut link, &mut cart, &snapshot)
                    {
                        match apply_command(
                            command,
                            &mut cart,
                            &mut audio,
                            &mut paused,
                            &mut bios_hle,
                            &mut frame_number,
                            CommandContext {
                                snapshot: &snapshot,
                                library_path: library_path.as_deref(),
                                audio_volume: &audio_volume,
                                screenshot_frames_left: &mut screenshot_frames_left,
                            },
                        ) {
                            CommandResult::Continue => {}
                            CommandResult::RestartFrame => {
                                next_frame_deadline = Instant::now();
                                continue;
                            }
                            CommandResult::Shutdown => break,
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            drive_link(&mut link, &mut cart, &snapshot);
            continue;
        }

        let speed = effective_speed(
            locked_speed.load(Ordering::Relaxed),
            turbo_ratio.load(Ordering::Relaxed),
            turbo_held.load(Ordering::Relaxed),
        );
        cart.as_mut()
            .expect("running cart exists")
            .bus
            .set_fast_forward(speed > 1);

        let frame_end = cart
            .as_ref()
            .expect("running cart exists")
            .bus
            .cycles()
            .saturating_add(CYCLES_PER_FRAME);
        while cart
            .as_ref()
            .is_some_and(|active| active.bus.cycles() < frame_end)
        {
            match receiver.try_recv() {
                Ok(command) => {
                    let Some(command) =
                        take_link_command(command, &mut link, &mut cart, &snapshot)
                    else {
                        continue;
                    };
                    match apply_command(
                    command,
                    &mut cart,
                    &mut audio,
                    &mut paused,
                    &mut bios_hle,
                    &mut frame_number,
                    CommandContext {
                        snapshot: &snapshot,
                        library_path: library_path.as_deref(),
                        audio_volume: &audio_volume,
                        screenshot_frames_left: &mut screenshot_frames_left,
                    },
                ) {
                    CommandResult::Continue => {}
                    CommandResult::RestartFrame => {
                        next_frame_deadline = Instant::now();
                        continue 'worker;
                    }
                    CommandResult::Shutdown => break 'worker,
                }
                }
                Err(mpsc::TryRecvError::Disconnected) => break 'worker,
                Err(mpsc::TryRecvError::Empty) => {}
            }

            if paused {
                continue 'worker;
            }
            let active = cart.as_mut().expect("running cart exists");
            active.bus.set_keys(held_keys(keys.load(Ordering::Relaxed), pad));
            if let Err(error) = step_quiet(&mut active.regs, &mut active.bus) {
                paused = true;
                let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                state.running = false;
                state.status = "ERROR".into();
                state.error = Some(error);
                continue 'worker;
            }
            link_steps += 1;
            let link_due = link_steps >= 2_000
                || cart
                    .as_ref()
                    .is_some_and(|active| active.bus.link_waiting());
            if link_due {
                link_steps = 0;
                if let Some(active) = cart.as_mut() {
                    link.service(&mut active.bus);
                }
            }
        }

        if cart.is_none() {
            continue;
        }
        frame_number += 1;
        let rgba = frame_to_rgba(cart.as_ref().expect("running cart exists").bus.frame());
        if let Some(left) = screenshot_frames_left {
            let left = left.saturating_sub(1);
            if left == 0 {
                screenshot_frames_left = None;
                if let Some(active) = cart.as_mut() {
                    if let Err(error) = store_generated_screenshot(
                        active,
                        &rgba,
                        &snapshot,
                        library_path.as_deref(),
                    ) {
                        eprintln!("{error}");
                    }
                }
            } else {
                screenshot_frames_left = Some(left);
            }
        }
        if let Some(active) = cart.as_mut() {
            link.service(&mut active.bus);
        }
        let link_view = link.view();
        let mut state = snapshot.lock().expect("emulator snapshot poisoned");
        let active = cart.as_ref().expect("running cart exists");
        state.title = active.title.clone();
        state.running = !paused;
        state.status = if paused { "PAUSED" } else { "RUNNING" }.into();
        state.frame_number = frame_number;
        state.image_version += 1;
        state.rgba = rgba;
        write_link_view(&mut state, &link_view);
        drop(state);

        let mut pace = frame_pace(frame_interval, speed);
        if speed == 1 && audio.is_some() {
            if let Some(active) = cart.as_ref() {
                let (queued, capacity, hz) = active.bus.audio_queue();
                pace = pace.mul_f64(audio_pace(queued, capacity, hz));
            }
        }
        next_frame_deadline += pace;
        let now = Instant::now();
        if next_frame_deadline > now {
            thread::sleep(next_frame_deadline - now);
        } else if now.duration_since(next_frame_deadline) > pace {
            next_frame_deadline = now;
        }
    }

    if let Some(active) = cart.as_ref() {
        if let Err(error) = save_cart(active) {
            eprintln!("{error}");
        }
    }
    drop(audio);
}

enum CommandResult {
    Continue,
    RestartFrame,
    Shutdown,
}

struct CommandContext<'a> {
    snapshot: &'a Arc<Mutex<Snapshot>>,
    library_path: Option<&'a Path>,
    audio_volume: &'a Arc<AtomicU8>,
    screenshot_frames_left: &'a mut Option<u32>,
}

fn apply_command(
    command: Command,
    cart: &mut Option<ActiveCart>,
    audio: &mut Option<AudioPlay>,
    paused: &mut bool,
    bios_hle: &mut bool,
    frame_number: &mut u64,
    context: CommandContext<'_>,
) -> CommandResult {
    let CommandContext {
        snapshot,
        library_path,
        audio_volume,
        screenshot_frames_left,
    } = context;
    match command {
        Command::Load(path) => match load_cart(path) {
            Ok(next) => {
                if let Some(current) = cart.as_ref() {
                    if let Err(error) = save_cart(current) {
                        set_error(snapshot, error);
                        return CommandResult::Continue;
                    }
                }
                let mut next = next;
                if let Some(library_path) = library_path {
                    let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                    let mut library = state.library.clone();
                    next.cover_path = library
                        .iter()
                        .find(|entry| entry.path == next.path)
                        .and_then(|entry| entry.cover_path.clone());
                    upsert_library_entry(&mut library, &next);
                    if let Err(error) = write_library(library_path, &library) {
                        drop(state);
                        set_error(snapshot, error);
                        return CommandResult::Continue;
                    }
                    state.library = library;
                    state.library_version += 1;
                }
                *audio = None;
                next.bus.set_bios_hle(*bios_hle);
                let audio_error = match next.bus.play_audio(Arc::clone(audio_volume)) {
                    Ok(player) => {
                        *audio = Some(player);
                        None
                    }
                    Err(error) => Some(error),
                };
                *frame_number = 0;
                let resume_prompt = match latest_state_path(&next.rom_id) {
                    Ok(path) => path.is_some(),
                    Err(error) => {
                        set_error(snapshot, error);
                        return CommandResult::Continue;
                    }
                };
                *paused = resume_prompt;
                *screenshot_frames_left = if resume_prompt {
                    None
                } else {
                    screenshot_frame_delay(next.cover_path.as_deref())
                };
                let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                state.title = next.title.clone();
                state.status = if resume_prompt {
                    "RESUME".into()
                } else {
                    "RUNNING".into()
                };
                state.running = !resume_prompt;
                state.resume_prompt = resume_prompt;
                state.boot_animation_started = (!resume_prompt).then(Instant::now);
                state.bios_hle = *bios_hle;
                state.error = audio_error
                    .map(|error| format!("游戏声音不可用：{error}"))
                    .or_else(|| state.library_error.clone());
                state.notice = if resume_prompt {
                    "已找到上次的即时存档".into()
                } else {
                    "ROM 已载入".into()
                };
                state.rom_path = Some(next.path.clone());
                state.state_available = resume_prompt;
                state.frame_number = 0;
                *cart = Some(next);
                CommandResult::RestartFrame
            }
            Err(error) => {
                set_error(snapshot, error);
                CommandResult::Continue
            }
        },
        Command::SetCover {
            rom_path,
            cover_path,
        } => {
            let Some(library_path) = library_path else {
                set_error(snapshot, "无法确定图库保存位置".into());
                return CommandResult::Continue;
            };
            match import_cover(&rom_path, &cover_path) {
                Ok(cover_path) => {
                    match set_library_cover(snapshot, library_path, &rom_path, cover_path.clone()) {
                        Ok(()) => {
                            if let Some(active) = cart.as_mut().filter(|cart| cart.path == rom_path)
                            {
                                active.cover_path = Some(cover_path);
                            }
                            let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                            state.error = state.library_error.clone();
                            state.notice = "卡带封面已更新".into();
                        }
                        Err(error) => set_error(snapshot, error),
                    }
                }
                Err(error) => set_error(snapshot, error),
            }
            CommandResult::Continue
        }
        Command::TogglePause => {
            if cart.is_some() {
                *paused = !*paused;
                let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                state.running = !*paused;
                state.status = if *paused { "PAUSED" } else { "RUNNING" }.into();
            }
            CommandResult::Continue
        }
        Command::StartFresh => {
            if cart.is_some() {
                *paused = false;
                *screenshot_frames_left = screenshot_frame_delay(
                    cart.as_ref()
                        .and_then(|active| active.cover_path.as_deref()),
                );
                let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                state.resume_prompt = false;
                state.boot_animation_started = Some(Instant::now());
                state.running = true;
                state.status = "RUNNING".into();
                state.notice = "从头开始".into();
                return CommandResult::RestartFrame;
            }
            CommandResult::Continue
        }
        Command::Reset => {
            let Some(current) = cart.as_ref() else {
                set_error(snapshot, "请先载入 ROM，再复位".into());
                return CommandResult::Continue;
            };
            if let Err(error) = save_cart(current) {
                set_error(snapshot, error);
                return CommandResult::Continue;
            }
            let path = current.path.clone();
            let cover_path = current.cover_path.clone();
            match load_cart(path) {
                Ok(mut next) => {
                    next.cover_path = cover_path;
                    next.bus.set_bios_hle(*bios_hle);
                    *audio = None;
                    let audio_error = match next.bus.play_audio(Arc::clone(audio_volume)) {
                        Ok(player) => {
                            *audio = Some(player);
                            None
                        }
                        Err(error) => Some(error),
                    };
                    *paused = false;
                    *frame_number = 0;
                    *screenshot_frames_left = screenshot_frame_delay(next.cover_path.as_deref());
                    let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                    state.title = next.title.clone();
                    state.status = "RUNNING".into();
                    state.running = true;
                    state.resume_prompt = false;
                    state.boot_animation_started = None;
                    state.bios_hle = *bios_hle;
                    state.error = audio_error
                        .map(|error| format!("游戏声音不可用：{error}"))
                        .or_else(|| state.library_error.clone());
                    state.notice = "已复位".into();
                    state.rom_path = Some(next.path.clone());
                    state.frame_number = 0;
                    *cart = Some(next);
                    CommandResult::RestartFrame
                }
                Err(error) => {
                    set_error(snapshot, error);
                    CommandResult::Continue
                }
            }
        }
        Command::ToggleBiosHle => {
            *bios_hle = !*bios_hle;
            if let Some(active) = cart.as_mut() {
                active.bus.set_bios_hle(*bios_hle);
            }
            let mut state = snapshot.lock().expect("emulator snapshot poisoned");
            state.bios_hle = *bios_hle;
            state.error = state.library_error.clone();
            state.notice = if *bios_hle {
                "BIOS HLE 快速路径已开启".into()
            } else {
                "BIOS HLE 快速路径已关闭".into()
            };
            CommandResult::Continue
        }
        command @ (Command::SaveStateSlot(_) | Command::QuickSaveState) => {
            let Some(active) = cart.as_ref() else {
                set_error(snapshot, "请先载入 ROM，再进行即时存档".into());
                return CommandResult::Continue;
            };
            let (path, label) = match command {
                Command::SaveStateSlot(slot) => match state_slot_path(&active.rom_id, slot) {
                    Ok(path) => (path, format!("即时存档槽 {slot}")),
                    Err(error) => {
                        set_error(snapshot, error);
                        return CommandResult::Continue;
                    }
                },
                Command::QuickSaveState => match quick_state_path(&active.rom_id) {
                    Ok(path) => (path, "快速即时存档".into()),
                    Err(error) => {
                        set_error(snapshot, error);
                        return CommandResult::Continue;
                    }
                },
                _ => unreachable!(),
            };
            match write_state_to(
                &active.rom_id,
                &active.path,
                &active.regs,
                &active.bus,
                *frame_number,
                &path,
            ) {
                Ok(()) => {
                    let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                    state.state_available = true;
                    state.error = state.library_error.clone();
                    state.notice = format!("{label}已保存");
                }
                Err(error) => set_error(snapshot, error),
            }
            CommandResult::Continue
        }
        command @ (Command::LoadStateSlot(_) | Command::QuickLoadState | Command::ResumeState) => {
            let resume = matches!(command, Command::ResumeState);
            let Some(active) = cart.as_ref() else {
                set_error(snapshot, "请先载入 ROM，再进行即时读档".into());
                return CommandResult::Continue;
            };
            let state_path = match command {
                Command::LoadStateSlot(slot) => match state_slot_path(&active.rom_id, slot) {
                    Ok(path) => Ok(Some(path)),
                    Err(error) => Err(error),
                },
                Command::QuickLoadState | Command::ResumeState => latest_state_path(&active.rom_id),
                _ => unreachable!(),
            };
            let state_path = match state_path {
                Ok(Some(path)) => path,
                Ok(None) => {
                    set_error(snapshot, "这张卡带还没有即时存档".into());
                    return CommandResult::Continue;
                }
                Err(error) => {
                    set_error(snapshot, error);
                    return CommandResult::Continue;
                }
            };
            match read_state(&state_path, active) {
                Ok((regs, bus, restored_frame_number)) => {
                    *audio = None;
                    let active = cart.as_mut().expect("loaded cart still exists");
                    active.regs = regs;
                    active.bus = bus;
                    *frame_number = restored_frame_number;
                    *bios_hle = active.bus.bios_hle();
                    let audio_error = match active.bus.play_audio(Arc::clone(audio_volume)) {
                        Ok(player) => {
                            *audio = Some(player);
                            None
                        }
                        Err(error) => Some(error),
                    };
                    if resume {
                        *paused = false;
                        *screenshot_frames_left =
                            screenshot_frame_delay(active.cover_path.as_deref());
                    }
                    let mut state = snapshot.lock().expect("emulator snapshot poisoned");
                    state.title = active.title.clone();
                    state.resume_prompt = false;
                    state.boot_animation_started = None;
                    state.running = !*paused;
                    state.status = if *paused { "PAUSED" } else { "RUNNING" }.into();
                    state.bios_hle = *bios_hle;
                    state.state_available = true;
                    state.error = audio_error
                        .map(|error| format!("游戏声音不可用：{error}"))
                        .or_else(|| state.library_error.clone());
                    state.notice = match command {
                        Command::ResumeState => "已从上次进度继续".into(),
                        Command::QuickLoadState => "已载入最近的即时存档".into(),
                        Command::LoadStateSlot(slot) => format!("即时存档槽 {slot} 已载入"),
                        _ => unreachable!(),
                    };
                    state.frame_number = *frame_number;
                    state.image_version += 1;
                    state.rgba = frame_to_rgba(active.bus.frame());
                    return CommandResult::RestartFrame;
                }
                Err(error) => set_error(snapshot, error),
            }
            CommandResult::Continue
        }
        Command::Error(error) => {
            set_error(snapshot, error);
            CommandResult::Continue
        }
        Command::HostLink | Command::JoinLink(_) | Command::StopLink | Command::SetLinkName(_) => {
            CommandResult::Continue
        }
        Command::Shutdown => CommandResult::Shutdown,
    }
}

fn set_error(snapshot: &Arc<Mutex<Snapshot>>, error: String) {
    let mut state = snapshot.lock().expect("emulator snapshot poisoned");
    if !state.running {
        state.status = "ERROR".into();
    }
    state.error = Some(error);
    state.notice.clear();
}

fn load_cart(path: PathBuf) -> Result<ActiveCart, String> {
    if !is_gba_rom(&path) {
        return Err(format!("只支持 .gba 卡带文件：{}", path.display()));
    }
    let path = std::fs::canonicalize(&path)
        .map_err(|error| format!("无法定位卡带 {}：{error}", path.display()))?;
    let rom =
        std::fs::read(&path).map_err(|error| format!("读不到卡带 {}：{error}", path.display()))?;
    let mut bus = Bus::new();
    bus.load_rom(&rom);
    let rom_id = rom_identity(&rom);
    let sav_path = path.with_extension("sav");
    if sav_path.is_file() {
        let bytes = std::fs::read(&sav_path)
            .map_err(|error| format!("读不到存档 {}：{error}", sav_path.display()))?;
        bus.load_backup(&bytes);
    }
    let rtc_path = path.with_extension("rtc");
    if rtc_path.is_file() {
        let bytes = std::fs::read(&rtc_path)
            .map_err(|error| format!("读不到时钟 {}：{error}", rtc_path.display()))?;
        bus.load_rtc(&bytes);
    }
    let mut regs = Registers::arm_start(0x0800_0000);
    regs.set_boot_stacks();
    let (title, game_code) = rom_metadata(&rom, &path);
    Ok(ActiveCart {
        path,
        rom_id,
        title,
        game_code,
        cover_path: None,
        regs,
        bus,
    })
}

fn is_gba_rom(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gba"))
}

fn application_data_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library").join("Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base.map(|path| path.join("gameboy-ultima"))
        .ok_or_else(|| "无法确定应用数据目录".to_string())
}

fn library_file_path() -> Result<PathBuf, String> {
    Ok(application_data_dir()?.join("library.json"))
}

fn cover_directory() -> Result<PathBuf, String> {
    Ok(application_data_dir()?.join("covers"))
}

fn read_library(path: &Path) -> Result<Vec<LibraryEntry>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取卡带图库 {} 失败：{error}", path.display()))?;
    let file: LibraryFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析卡带图库 {} 失败：{error}", path.display()))?;
    if file.version != 1 {
        return Err(format!("不支持的卡带图库版本：{}", file.version));
    }
    Ok(file.entries)
}

fn write_library(path: &Path, entries: &[LibraryEntry]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("卡带图库路径无效：{}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建卡带图库目录 {} 失败：{error}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(&LibraryFile {
        version: 1,
        entries: entries.to_vec(),
    })
    .map_err(|error| format!("序列化卡带图库失败：{error}"))?;
    let temp_path = path.with_extension("json.tmp");
    std::fs::write(&temp_path, bytes)
        .map_err(|error| format!("写卡带图库 {} 失败：{error}", temp_path.display()))?;
    std::fs::rename(&temp_path, path)
        .map_err(|error| format!("保存卡带图库 {} 失败：{error}", path.display()))
}

fn upsert_library_entry(library: &mut Vec<LibraryEntry>, cart: &ActiveCart) {
    let entry = LibraryEntry {
        path: cart.path.clone(),
        title: cart.title.clone(),
        game_code: cart.game_code.clone(),
        cover_path: cart.cover_path.clone(),
    };
    if let Some(existing) = library.iter_mut().find(|item| item.path == cart.path) {
        let cover_path = existing.cover_path.clone().or(entry.cover_path);
        *existing = LibraryEntry {
            cover_path,
            ..entry
        };
    } else {
        library.push(entry);
    }
}

fn set_library_cover(
    snapshot: &Arc<Mutex<Snapshot>>,
    library_path: &Path,
    rom_path: &Path,
    cover_path: PathBuf,
) -> Result<(), String> {
    let mut state = snapshot.lock().expect("emulator snapshot poisoned");
    let mut library = state.library.clone();
    let entry = library
        .iter_mut()
        .find(|entry| entry.path == rom_path)
        .ok_or_else(|| format!("图库中找不到卡带：{}", rom_path.display()))?;
    entry.cover_path = Some(cover_path);
    write_library(library_path, &library)?;
    state.library = library;
    state.library_version += 1;
    Ok(())
}

fn rom_metadata(rom: &[u8], path: &Path) -> (String, String) {
    let decode_field = |field: &[u8]| {
        field
            .iter()
            .copied()
            .take_while(|byte| *byte != 0)
            .filter(|byte| byte.is_ascii_graphic() || *byte == b' ')
            .map(char::from)
            .collect::<String>()
            .trim()
            .to_owned()
    };
    let title = rom
        .get(0xA0..0xAC)
        .map(decode_field)
        .filter(|title| !title.is_empty())
        .or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "未知卡带".into());
    let game_code = rom.get(0xAC..0xB0).map(decode_field).unwrap_or_default();
    (title, game_code)
}

fn rom_cover_id(path: &Path) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in path.as_os_str().to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn is_custom_cover(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("-custom.png"))
}

fn screenshot_frame_delay(cover_path: Option<&Path>) -> Option<u32> {
    if cover_path.is_some_and(is_custom_cover) {
        return None;
    }
    if cover_path.is_some_and(|path| path.is_file()) {
        return None;
    }
    Some(SCREENSHOT_FRAME_DELAY)
}

fn compose_screenshot_cover(frame: &[u8]) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let mut image =
        ImageBuffer::from_pixel(COVER_WIDTH, COVER_HEIGHT, Rgba([0x0B, 0x10, 0x12, 0xFF]));
    let accent = Rgba([0xC8, 0xF1, 0x69, 0xFF]);
    for y in COVER_ACCENT_TOP..COVER_ACCENT_TOP + COVER_ACCENT_HEIGHT {
        for x in 0..COVER_WIDTH {
            image.put_pixel(x, y, accent);
        }
    }
    if frame.len() != WIDTH * HEIGHT * 4 {
        return image;
    }
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = (y * WIDTH + x) * 4;
            let pixel = Rgba([frame[index], frame[index + 1], frame[index + 2], 0xFF]);
            let dx = (x as u32) * 2;
            let dy = COVER_SCREEN_TOP + (y as u32) * 2;
            image.put_pixel(dx, dy, pixel);
            image.put_pixel(dx + 1, dy, pixel);
            image.put_pixel(dx, dy + 1, pixel);
            image.put_pixel(dx + 1, dy + 1, pixel);
        }
    }
    image
}

fn generated_cover_path(rom_path: &Path) -> Result<PathBuf, String> {
    let directory = cover_directory()?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("创建封面目录 {} 失败：{error}", directory.display()))?;
    Ok(directory.join(format!("{}-generated.png", rom_cover_id(rom_path))))
}

fn store_generated_screenshot(
    cart: &mut ActiveCart,
    frame: &[u8],
    snapshot: &Arc<Mutex<Snapshot>>,
    library_path: Option<&Path>,
) -> Result<(), String> {
    if screenshot_frame_delay(cart.cover_path.as_deref()).is_none() {
        return Ok(());
    }
    let path = generated_cover_path(&cart.path)?;
    compose_screenshot_cover(frame)
        .save(&path)
        .map_err(|error| format!("保存游戏画面 {} 失败：{error}", path.display()))?;
    cart.cover_path = Some(path.clone());
    if let Some(library_path) = library_path {
        set_library_cover(snapshot, library_path, &cart.path, path)?;
    }
    Ok(())
}

fn import_cover(rom_path: &Path, source_path: &Path) -> Result<PathBuf, String> {
    let cover = image::open(source_path)
        .map_err(|error| format!("无法读取封面图片 {}：{error}", source_path.display()))?
        .resize_to_fill(480, 640, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let directory = cover_directory()?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("创建封面目录 {} 失败：{error}", directory.display()))?;
    let path = directory.join(format!("{}-custom.png", rom_cover_id(rom_path)));
    cover
        .save(&path)
        .map_err(|error| format!("保存封面图片 {} 失败：{error}", path.display()))?;
    Ok(path)
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u8,
    rom_id: String,
    regs: Registers,
    bus: Bus,
    frame_number: u64,
}

fn rom_identity(rom: &[u8]) -> String {
    format!("{:x}", Sha256::digest(rom))
}

fn state_directory(rom_id: &str) -> Result<PathBuf, String> {
    Ok(application_data_dir()?.join("save_states").join(rom_id))
}

fn slot_state_file_name(slot: u8) -> Result<String, String> {
    match slot {
        1..=3 => Ok(format!("slot-{slot}.state")),
        _ => Err(format!("无效的即时存档槽位：{slot}")),
    }
}

fn state_slot_path(rom_id: &str, slot: u8) -> Result<PathBuf, String> {
    state_slot_path_in(&state_directory(rom_id)?, slot)
}

fn quick_state_path(rom_id: &str) -> Result<PathBuf, String> {
    Ok(quick_state_path_in(&state_directory(rom_id)?))
}

fn quick_state_path_in(directory: &Path) -> PathBuf {
    directory.join("quick.state")
}

fn latest_state_path(rom_id: &str) -> Result<Option<PathBuf>, String> {
    latest_state_path_in(&state_directory(rom_id)?)
}

fn state_slot_path_in(directory: &Path, slot: u8) -> Result<PathBuf, String> {
    Ok(directory.join(slot_state_file_name(slot)?))
}

fn latest_state_path_in(directory: &Path) -> Result<Option<PathBuf>, String> {
    let paths = [
        state_slot_path_in(directory, 1)?,
        state_slot_path_in(directory, 2)?,
        state_slot_path_in(directory, 3)?,
        quick_state_path_in(directory),
    ];
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
    for path in paths {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!("读取即时存档信息 {} 失败：{error}", path.display()));
            }
        };
        let modified = metadata
            .modified()
            .map_err(|error| format!("读取即时存档时间 {} 失败：{error}", path.display()))?;
        if latest.as_ref().is_none_or(|(time, _)| modified > *time) {
            latest = Some((modified, path));
        }
    }
    Ok(latest.map(|(_, path)| path))
}

fn write_state_to(
    rom_id: &str,
    rom_path: &Path,
    regs: &Registers,
    bus: &Bus,
    frame_number: u64,
    path: &Path,
) -> Result<(), String> {
    let state = StateFile {
        version: 1,
        rom_id: rom_id.to_owned(),
        regs: regs.clone(),
        bus: bus.clone(),
        frame_number,
    };
    let bytes =
        serde_json::to_vec(&state).map_err(|error| format!("序列化即时存档失败：{error}"))?;
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("state");
    let temp_path = path.with_extension(format!("{extension}.tmp"));
    let parent = path
        .parent()
        .ok_or_else(|| format!("即时存档路径无效：{}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建即时存档目录 {} 失败：{error}", parent.display()))?;
    std::fs::write(&temp_path, bytes)
        .map_err(|error| format!("写即时存档 {} 失败：{error}", temp_path.display()))?;
    std::fs::rename(&temp_path, &path).map_err(|error| {
        format!(
            "保存 ROM {} 的即时存档 {} 失败：{error}",
            rom_path.display(),
            path.display()
        )
    })
}

fn read_state(
    path: &std::path::Path,
    current: &ActiveCart,
) -> Result<(Registers, Bus, u64), String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读即时存档 {} 失败：{error}", path.display()))?;
    let state: StateFile =
        serde_json::from_slice(&bytes).map_err(|error| format!("即时存档格式无效：{error}"))?;
    if state.version != 1 {
        return Err(format!("不支持的即时存档版本：{}", state.version));
    }
    if state.rom_id != current.rom_id {
        return Err("即时存档对应的 ROM 与当前载入的 ROM 不一致".into());
    }
    if !current.bus.same_rom(&state.bus) {
        return Err("即时存档对应的 ROM 与当前载入的 ROM 不一致".into());
    }
    Ok((state.regs, state.bus, state.frame_number))
}

fn save_cart(cart: &ActiveCart) -> Result<(), String> {
    if cart.bus.backup_dirty() {
        let sav_path = cart.path.with_extension("sav");
        std::fs::write(&sav_path, cart.bus.backup_bytes())
            .map_err(|error| format!("写存档 {} 失败：{error}", sav_path.display()))?;
    }
    if let Some(bytes) = cart.bus.rtc_bytes() {
        let rtc_path = cart.path.with_extension("rtc");
        std::fs::write(&rtc_path, bytes)
            .map_err(|error| format!("写时钟 {} 失败：{error}", rtc_path.display()))?;
    }
    Ok(())
}

fn frame_to_rgba(frame: &gameboy_ultima::Frame) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(WIDTH * HEIGHT * 4);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let color = rgb555_to_888(frame.pixel(x, y));
            rgba.extend_from_slice(&[(color >> 16) as u8, (color >> 8) as u8, color as u8, 0xFF]);
        }
    }
    rgba
}

fn picture_menu_item(mode: PictureMode, open: bool) -> MenuItem {
    let name = picture_option_label(mode.title(), open);
    match mode {
        #[cfg(target_os = "macos")]
        PictureMode::Metal => MenuItem::action(name, ToggleMetalScreen),
        PictureMode::Sharpen => MenuItem::action(name, ToggleSharpenScreen),
        PictureMode::CpuNearest => MenuItem::action(name, ToggleCpuScreen),
        PictureMode::Linear => MenuItem::action(name, ToggleLinearScreen),
    }
}

fn speed_choice_menu(name: &'static str, selected: u8, locked: bool) -> MenuItem {
    let items = SPEEDS
        .into_iter()
        .map(|speed| {
            let label = picture_option_label(&format!("{speed}×"), speed == selected);
            if locked {
                MenuItem::action(label, SetLockedSpeed(speed))
            } else {
                MenuItem::action(label, SetTurboSpeed(speed))
            }
        })
        .collect();
    MenuItem::submenu(Menu {
        name: name.into(),
        items,
    })
}

fn app_menus(open: &[PictureMode], speed: u8, turbo: u8) -> Vec<Menu> {
    let mut compare_items: Vec<MenuItem> = picture_modes()
        .into_iter()
        .map(|mode| picture_menu_item(mode, open.contains(&mode)))
        .collect();
    compare_items.push(MenuItem::separator());
    compare_items.push(MenuItem::action("全部打开", CompareScreens));
    vec![
        Menu {
            name: "ULTIMA".into(),
            items: vec![MenuItem::action("退出 ULTIMA", Quit)],
        },
        Menu {
            name: "文件".into(),
            items: vec![
                MenuItem::action("打开 ROM…", OpenRom),
                MenuItem::action("打开图库", ShowLibrary),
                MenuItem::action("设置当前卡带封面…", ChooseCover),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: "即时存档".into(),
                    items: vec![
                        MenuItem::action("存档槽 1", SaveSlot1),
                        MenuItem::action("存档槽 2", SaveSlot2),
                        MenuItem::action("存档槽 3", SaveSlot3),
                        MenuItem::separator(),
                        MenuItem::action("快速即时存档", QuickSaveState),
                    ],
                }),
                MenuItem::submenu(Menu {
                    name: "即时读档".into(),
                    items: vec![
                        MenuItem::action("存档槽 1", LoadSlot1),
                        MenuItem::action("存档槽 2", LoadSlot2),
                        MenuItem::action("存档槽 3", LoadSlot3),
                        MenuItem::separator(),
                        MenuItem::action("快速即时读档（最近存档）", QuickLoadState),
                    ],
                }),
            ],
        },
        Menu {
            name: "模拟".into(),
            items: vec![
                MenuItem::action("暂停 / 继续", TogglePause),
                MenuItem::action("复位", ResetConsole),
                speed_choice_menu("速度", speed, true),
                speed_choice_menu("按住加速", turbo, false),
                MenuItem::action("键位设置…", ShowControls),
                MenuItem::action("联机…", ShowLink),
                MenuItem::action("切换 BIOS HLE", ToggleBiosHle),
                MenuItem::separator(),
                MenuItem::action("声音：音量 +10%", VolumeUp),
                MenuItem::action("声音：音量 -10%", VolumeDown),
                MenuItem::action("声音：静音 / 取消静音", ToggleMute),
            ],
        },
        Menu {
            name: "视图".into(),
            items: vec![
                MenuItem::action("显示游戏画面", ShowGame),
                MenuItem::action("显示图库", ShowLibrary),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: "对比画面".into(),
                    items: compare_items,
                }),
            ],
        },
    ]
}

fn accept_chosen_rom(
    commands: mpsc::Sender<Command>,
    library_mode: Arc<AtomicBool>,
    path: PathBuf,
) {
    if !is_gba_rom(&path) {
        let _ = commands.send(Command::Error(format!(
            "只支持 .gba 卡带文件：{}",
            path.display()
        )));
        return;
    }
    library_mode.store(false, Ordering::Relaxed);
    let _ = commands.send(Command::Load(path));
}

#[cfg(target_os = "macos")]
async fn pick_gba_rom() -> Option<PathBuf> {
    // The panel has to open after the click handler returns. A synchronous
    // NSOpenPanel::runModal re-enters AppKit while GPUI still holds that
    // borrow and aborts with panic_already_mutably_borrowed.
    rfd::AsyncFileDialog::new()
        .add_filter("GBA 卡带", &["gba"])
        .set_title("选择 GBA 卡带")
        .pick_file()
        .await
        .map(|handle| handle.path().to_path_buf())
}

fn prompt_rom_from_menu(
    cx: &mut Context<AppView>,
    commands: mpsc::Sender<Command>,
    library_mode: Arc<AtomicBool>,
) {
    #[cfg(target_os = "macos")]
    {
        cx.spawn(async move |_, _| {
            if let Some(path) = pick_gba_rom().await {
                accept_chosen_rom(commands, library_mode, path);
            }
        })
        .detach();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 GBA 卡带".into()),
        });
        cx.spawn(async move |_, _| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    accept_chosen_rom(commands, library_mode, path);
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                let _ = commands.send(Command::Error(format!("打开文件选择器失败：{error}")));
            }
            Err(error) => {
                let _ = commands.send(Command::Error(format!("文件选择已取消：{error}")));
            }
        })
        .detach();
    }
}

fn choose_rom_from_menu(
    cx: &mut App,
    commands: mpsc::Sender<Command>,
    library_mode: Arc<AtomicBool>,
) {
    #[cfg(target_os = "macos")]
    {
        cx.spawn(async move |_| {
            if let Some(path) = pick_gba_rom().await {
                accept_chosen_rom(commands, library_mode, path);
            }
        })
        .detach();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 GBA 卡带".into()),
        });
        cx.spawn(async move |_| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    accept_chosen_rom(commands, library_mode, path);
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                let _ = commands.send(Command::Error(format!("打开文件选择器失败：{error}")));
            }
            Err(error) => {
                let _ = commands.send(Command::Error(format!("文件选择已取消：{error}")));
            }
        })
        .detach();
    }
}

fn choose_cover_from_menu(
    cx: &mut App,
    commands: mpsc::Sender<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
) {
    let rom_path = snapshot
        .lock()
        .expect("emulator snapshot poisoned")
        .rom_path
        .clone();
    let Some(rom_path) = rom_path else {
        let _ = commands.send(Command::Error("请先加载一张卡带，再设置封面".into()));
        return;
    };
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("选择卡带封面图片".into()),
    });
    cx.spawn(async move |_| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(cover_path) = paths.into_iter().next() {
                let _ = commands.send(Command::SetCover {
                    rom_path,
                    cover_path,
                });
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => {
            let _ = commands.send(Command::Error(format!("打开图片选择器失败：{error}")));
        }
        Err(error) => {
            let _ = commands.send(Command::Error(format!("图片选择已取消：{error}")));
        }
    })
    .detach();
}

#[derive(Clone)]
struct WindowLaunch {
    snapshot: Arc<Mutex<Snapshot>>,
    commands: mpsc::Sender<Command>,
    keys: Arc<AtomicU16>,
    locked_speed: Arc<AtomicU8>,
    turbo_ratio: Arc<AtomicU8>,
    turbo_held: Arc<AtomicBool>,
    library_mode: Arc<AtomicBool>,
    controls: Arc<Mutex<ControlSettings>>,
    link_ui: Arc<Mutex<LinkUi>>,
    windows_alive: Arc<AtomicUsize>,
    picture_windows: Arc<Mutex<PictureWindows>>,
    chrome_dirty: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
struct PictureSlot {
    token: u64,
    handle: gpui::WindowHandle<AppView>,
}

struct PictureWindows {
    next_token: u64,
    #[cfg(target_os = "macos")]
    metal: Option<PictureSlot>,
    #[cfg(target_os = "macos")]
    sharpen: Option<PictureSlot>,
    cpu: Option<PictureSlot>,
    linear: Option<PictureSlot>,
}

impl PictureWindows {
    fn alloc_token(&mut self) -> u64 {
        self.next_token = self.next_token.saturating_add(1);
        self.next_token
    }

    fn slot(&self, mode: PictureMode) -> Option<PictureSlot> {
        match mode {
            #[cfg(target_os = "macos")]
            PictureMode::Metal => self.metal,
            PictureMode::Sharpen => self.sharpen,
            PictureMode::CpuNearest => self.cpu,
            PictureMode::Linear => self.linear,
        }
    }

    fn slot_mut(&mut self, mode: PictureMode) -> &mut Option<PictureSlot> {
        match mode {
            #[cfg(target_os = "macos")]
            PictureMode::Metal => &mut self.metal,
            PictureMode::Sharpen => &mut self.sharpen,
            PictureMode::CpuNearest => &mut self.cpu,
            PictureMode::Linear => &mut self.linear,
        }
    }

    fn insert(&mut self, mode: PictureMode, slot: PictureSlot) {
        *self.slot_mut(mode) = Some(slot);
    }

    fn take_token(&mut self, mode: PictureMode, token: u64) -> bool {
        let slot = self.slot_mut(mode);
        if slot.is_some_and(|slot| slot.token == token) {
            *slot = None;
            true
        } else {
            false
        }
    }
}

fn launch_arguments() -> (Option<PathBuf>, bool) {
    let mut rom = None;
    let mut compare = false;
    for arg in std::env::args_os().skip(1) {
        if arg == "--compare" {
            compare = true;
        } else if rom.is_none() {
            rom = Some(PathBuf::from(arg));
        }
    }
    (rom, compare)
}

/// Three equal frames inside the display. A wide screen gets one row.
/// A narrower screen puts the third frame on the next row.
fn comparison_tiles(display_w: f32, display_h: f32) -> [(f32, f32, f32, f32); 3] {
    let margin_x = 16.0;
    let margin_top = 40.0;
    let margin_bottom = 24.0;
    let gap = 12.0;
    let min_w = 480.0;
    let display_w = display_w.max(1.0);
    let display_h = display_h.max(1.0);
    let usable_w = (display_w - margin_x * 2.0).max(min_w);
    let usable_h = (display_h - margin_top - margin_bottom).max(320.0);

    let three_w = (usable_w - gap * 2.0) / 3.0;
    if three_w >= min_w {
        let width = three_w.min(720.0);
        let height = usable_h.min(760.0);
        let row_w = width * 3.0 + gap * 2.0;
        let x0 = ((display_w - row_w) * 0.5).max(0.0);
        let y = margin_top.min((display_h - height).max(0.0));
        return [
            (x0, y, width, height),
            (x0 + width + gap, y, width, height),
            (x0 + (width + gap) * 2.0, y, width, height),
        ];
    }

    let width = ((usable_w - gap) * 0.5).clamp(320.0, 720.0);
    let preferred_h = ((usable_h - gap) * 0.5).clamp(320.0, 560.0);
    let height = preferred_h.min(((display_h - gap) * 0.5).max(1.0));
    let row_w = width * 2.0 + gap;
    let x0 = ((display_w - row_w) * 0.5).max(0.0);
    let y0 = margin_top.min((display_h - height * 2.0 - gap).max(0.0));
    [
        (x0, y0, width, height),
        (x0 + width + gap, y0, width, height),
        (x0, y0 + height + gap, width, height),
    ]
}

fn layout_tiles(count: usize, display_w: f32, display_h: f32) -> Vec<(f32, f32, f32, f32)> {
    let count = count.clamp(1, 4);
    if count == 3 {
        return comparison_tiles(display_w, display_h).to_vec();
    }
    let margin_x = 16.0;
    let margin_top = 40.0;
    let margin_bottom = 24.0;
    let gap = 12.0;
    let min_w = 480.0;
    let display_w = display_w.max(1.0);
    let display_h = display_h.max(1.0);
    let usable_w = (display_w - margin_x * 2.0).max(min_w);
    let usable_h = (display_h - margin_top - margin_bottom).max(320.0);
    if count == 4 {
        let four_w = (usable_w - gap * 3.0) / 4.0;
        if four_w >= min_w {
            let width = four_w.min(720.0);
            let height = usable_h.min(760.0);
            let row_w = width * 4.0 + gap * 3.0;
            let x0 = ((display_w - row_w) * 0.5).max(0.0);
            let y = margin_top.min((display_h - height).max(0.0));
            return (0..4)
                .map(|column| (x0 + (width + gap) * column as f32, y, width, height))
                .collect();
        }
        let width = ((usable_w - gap) * 0.5).clamp(320.0, 720.0);
        let height = ((usable_h - gap) * 0.5)
            .clamp(320.0, 560.0)
            .min(((display_h - gap) * 0.5).max(1.0));
        let row_w = width * 2.0 + gap;
        let x0 = ((display_w - row_w) * 0.5).max(0.0);
        let y0 = margin_top.min((display_h - height * 2.0 - gap).max(0.0));
        return vec![
            (x0, y0, width, height),
            (x0 + width + gap, y0, width, height),
            (x0, y0 + height + gap, width, height),
            (x0 + width + gap, y0 + height + gap, width, height),
        ];
    }
    if count == 1 {
        let width = usable_w.min(1040.0);
        let height = usable_h.min(720.0);
        let x = ((display_w - width) * 0.5).max(0.0);
        let y = margin_top.min((display_h - height).max(0.0));
        return vec![(x, y, width, height)];
    }
    let two_w = (usable_w - gap) / 2.0;
    if two_w >= min_w {
        let width = two_w.min(720.0);
        let height = usable_h.min(760.0);
        let row_w = width * 2.0 + gap;
        let x0 = ((display_w - row_w) * 0.5).max(0.0);
        let y = margin_top.min((display_h - height).max(0.0));
        return vec![(x0, y, width, height), (x0 + width + gap, y, width, height)];
    }
    let width = usable_w.min(720.0);
    let height = ((usable_h - gap) * 0.5).clamp(320.0, 560.0);
    let x = ((display_w - width) * 0.5).max(0.0);
    let y0 = margin_top.min((display_h - height * 2.0 - gap).max(0.0));
    vec![(x, y0, width, height), (x, y0 + height + gap, width, height)]
}

fn layout_frames(count: usize, display_w: f32, display_h: f32) -> Vec<Bounds<Pixels>> {
    layout_tiles(count, display_w, display_h)
        .into_iter()
        .map(|(x, y, width, height)| Bounds {
            origin: point(px(x), px(y)),
            size: size(px(width), px(height)),
        })
        .collect()
}

fn display_point_size(cx: &App) -> (f32, f32) {
    cx.primary_display()
        .map(|display| {
            let bounds = display.bounds();
            (bounds.size.width / px(1.), bounds.size.height / px(1.))
        })
        .unwrap_or((1440., 900.))
}

#[cfg(target_os = "macos")]
fn place_window(window: &mut Window, bounds: Bounds<Pixels>) {
    use cocoa::appkit::{NSScreen, NSWindow};
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSArray, NSPoint, NSSize};
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view = appkit.ns_view.as_ptr() as id;
    unsafe {
        let ns_window: id = msg_send![view, window];
        if ns_window == nil {
            return;
        }
        let screens = NSScreen::screens(nil);
        if NSArray::count(screens) == 0 {
            return;
        }
        let screen = NSArray::objectAtIndex(screens, 0);
        let screen_frame = NSScreen::frame(screen);
        let x = (bounds.origin.x / px(1.)) as f64;
        let y = (bounds.origin.y / px(1.)) as f64;
        ns_window.setContentSize_(NSSize::new(
            (bounds.size.width / px(1.)) as f64,
            (bounds.size.height / px(1.)) as f64,
        ));
        ns_window.setFrameTopLeftPoint_(NSPoint::new(
            screen_frame.origin.x + x,
            screen_frame.origin.y + screen_frame.size.height - y,
        ));
    }
}

fn open_game_window(
    cx: &mut App,
    bounds: Bounds<Pixels>,
    title: &str,
    focus: bool,
    picture: PictureMode,
    compare: bool,
    picture_token: u64,
    launch: WindowLaunch,
) -> gpui::WindowHandle<AppView> {
    let title = title.to_string();
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(320.))),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(title.clone().into()),
                ..Default::default()
            }),
            focus,
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title(&title);
            cx.new(|cx| {
                AppView::new(
                    window,
                    cx,
                    launch.snapshot,
                    launch.commands,
                    launch.keys,
                    Arc::clone(&launch.locked_speed),
                    Arc::clone(&launch.turbo_ratio),
                    Arc::clone(&launch.turbo_held),
                    launch.library_mode,
                    launch.controls,
                    launch.link_ui,
                    launch.windows_alive,
                    launch.picture_windows,
                    launch.chrome_dirty,
                    picture_token,
                    picture,
                    compare,
                )
            })
        },
    )
    .expect("open window")
}

fn window_still_open(handle: gpui::WindowHandle<AppView>, cx: &mut App) -> bool {
    handle.is_active(cx).is_some()
}

fn picture_candidates(windows: &PictureWindows) -> Vec<(PictureMode, PictureSlot)> {
    picture_modes()
        .into_iter()
        .filter_map(|mode| windows.slot(mode).map(|slot| (mode, slot)))
        .collect()
}

fn open_picture_mode(cx: &mut App, mode: PictureMode, launch: &WindowLaunch) {
    let token = launch
        .picture_windows
        .lock()
        .expect("picture windows poisoned")
        .alloc_token();
    let (display_w, display_h) = display_point_size(cx);
    let mut modes = {
        let candidates = {
            let windows = launch
                .picture_windows
                .lock()
                .expect("picture windows poisoned");
            picture_candidates(&windows)
        };
        candidates
            .into_iter()
            .filter(|(_, slot)| window_still_open(slot.handle, cx))
            .map(|(mode, _)| mode)
            .collect::<Vec<_>>()
    };
    if !modes.contains(&mode) {
        modes.push(mode);
    }
    modes.sort_by_key(|mode| mode.order());
    let index = modes.iter().position(|item| *item == mode).unwrap_or(0);
    let frames = layout_frames(modes.len(), display_w, display_h);
    let bounds = frames[index];
    let handle = open_game_window(
        cx,
        bounds,
        &format!("ULTIMA · {}", mode.title()),
        false,
        mode,
        true,
        token,
        launch.clone(),
    );
    launch
        .picture_windows
        .lock()
        .expect("picture windows poisoned")
        .insert(mode, PictureSlot { token, handle });
}

fn apply_speed_choice(launch: &WindowLaunch, locked: Option<u8>, turbo: Option<u8>) {
    let mut settings = launch.controls.lock().expect("control settings poisoned");
    if let Some(speed) = locked {
        let speed = normalize_speed(speed);
        settings.speed = speed;
        launch.locked_speed.store(speed, Ordering::Relaxed);
    }
    if let Some(speed) = turbo {
        let speed = normalize_turbo(speed);
        settings.turbo = speed;
        launch.turbo_ratio.store(speed, Ordering::Relaxed);
    }
    let saved = settings.saved();
    drop(settings);
    let Ok(path) = controls_file_path() else {
        return;
    };
    if let Err(error) = write_controls(&path, &saved) {
        let _ = launch.commands.send(Command::Error(error));
    }
}

fn refresh_picture_chrome(
    cx: &mut App,
    picture_windows: &Mutex<PictureWindows>,
    controls: &Mutex<ControlSettings>,
) {
    let live = {
        let windows = picture_windows.lock().expect("picture windows poisoned");
        picture_candidates(&windows)
    };
    let live: Vec<_> = live
        .into_iter()
        .filter(|(_, slot)| window_still_open(slot.handle, cx))
        .collect();
    let has_shell = live.iter().any(|(_, slot)| {
        slot.handle
            .read(cx)
            .map(|view| !view.compare)
            .unwrap_or(false)
    });
    if !has_shell {
        if let Some((_, slot)) = live.first() {
            let _ = slot.handle.update(cx, |view, _, cx| {
                view.compare = false;
                cx.notify();
            });
        }
    }
    let multiple = live.len() > 1;
    let frames = if multiple {
        let (display_w, display_h) = display_point_size(cx);
        Some(layout_frames(live.len(), display_w, display_h))
    } else {
        None
    };
    for (index, (_, slot)) in live.iter().enumerate() {
        let _ = slot.handle.update(cx, |view, window, cx| {
            view.labeled = multiple;
            let title = if multiple {
                format!("ULTIMA · {}", view.picture.title())
            } else {
                "ULTIMA".to_string()
            };
            window.set_window_title(&title);
            #[cfg(target_os = "macos")]
            if let Some(frames) = frames.as_ref() {
                place_window(window, frames[index]);
            }
            cx.notify();
        });
    }
    let open = live.iter().map(|(mode, _)| *mode).collect::<Vec<_>>();
    let (speed, turbo) = {
        let settings = controls.lock().expect("control settings poisoned");
        (settings.speed, settings.turbo)
    };
    cx.set_menus(app_menus(&open, speed, turbo));
}

fn toggle_picture(cx: &mut App, mode: PictureMode, launch: &WindowLaunch) {
    let (choice, handle, sole) = {
        let candidates = {
            let windows = launch
                .picture_windows
                .lock()
                .expect("picture windows poisoned");
            picture_candidates(&windows)
        };
        let live: Vec<_> = candidates
            .into_iter()
            .filter(|(_, slot)| window_still_open(slot.handle, cx))
            .collect();
        let handle = live
            .iter()
            .find(|(item, _)| *item == mode)
            .map(|(_, slot)| slot.handle);
        let sole = (live.len() == 1).then(|| live[0].1.handle);
        (picture_toggle(live.len(), handle.is_some()), handle, sole)
    };
    match choice {
        PictureToggle::Focus => {
            if let Some(handle) = handle {
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
        }
        PictureToggle::Switch => {
            if let Some(handle) = sole {
                let _ = handle.update(cx, |view, window, cx| {
                    view.adopt_picture(mode, window);
                    cx.notify();
                });
            }
            refresh_picture_chrome(cx, &launch.picture_windows, &launch.controls);
        }
        PictureToggle::Close => {
            let slot = launch
                .picture_windows
                .lock()
                .expect("picture windows poisoned")
                .slot_mut(mode)
                .take();
            if let Some(slot) = slot {
                let was_shell = slot
                    .handle
                    .read(cx)
                    .map(|view| !view.compare)
                    .unwrap_or(false);
                if was_shell {
                    let candidates = {
                        let windows = launch
                            .picture_windows
                            .lock()
                            .expect("picture windows poisoned");
                        picture_candidates(&windows)
                    };
                    let replacement = candidates
                        .into_iter()
                        .filter(|(_, slot)| window_still_open(slot.handle, cx))
                        .find(|(other, _)| *other != mode)
                        .map(|(_, slot)| slot.handle);
                    if let Some(replacement) = replacement {
                        let _ = replacement.update(cx, |view, _, cx| {
                            view.compare = false;
                            cx.notify();
                        });
                    }
                }
                let _ = slot
                    .handle
                    .update(cx, |_, window, _| window.remove_window());
            }
            refresh_picture_chrome(cx, &launch.picture_windows, &launch.controls);
        }
        PictureToggle::Open => {
            open_picture_mode(cx, mode, launch);
            refresh_picture_chrome(cx, &launch.picture_windows, &launch.controls);
        }
    }
}

fn open_screen_comparison(cx: &mut App, launch: &WindowLaunch) {
    for mode in picture_modes() {
        let open = {
            let windows = launch
                .picture_windows
                .lock()
                .expect("picture windows poisoned");
            windows
                .slot(mode)
                .is_some_and(|slot| window_still_open(slot.handle, cx))
        };
        if !open {
            open_picture_mode(cx, mode, launch);
        }
    }
    refresh_picture_chrome(cx, &launch.picture_windows, &launch.controls);
}

fn main() {
    let saved = load_saved_controls();
    let locked_speed = Arc::new(AtomicU8::new(saved.speed));
    let turbo_ratio = Arc::new(AtomicU8::new(saved.turbo));
    let turbo_held = Arc::new(AtomicBool::new(false));
    let (snapshot, commands, keys, audio_volume, worker) = start_emulator(
        Arc::clone(&locked_speed),
        Arc::clone(&turbo_ratio),
        Arc::clone(&turbo_held),
    );
    let (launch_rom, compare_on_launch) = launch_arguments();
    if let Some(path) = &launch_rom {
        let _ = commands.send(Command::Load(path.clone()));
    }
    let library_mode = Arc::new(AtomicBool::new(launch_rom.is_none()));
    let controls = Arc::new(Mutex::new(ControlSettings {
        open: false,
        capturing: None,
        map: saved.map,
        speed: saved.speed,
        turbo: saved.turbo,
    }));
    let saved_link_name = load_link_name();
    if !saved_link_name.is_empty() {
        let _ = commands.send(Command::SetLinkName(saved_link_name.clone()));
    }
    let link_ui = Arc::new(Mutex::new(LinkUi {
        open: false,
        address: String::new(),
        name: saved_link_name.clone(),
        name_touched: !saved_link_name.is_empty(),
        field: LinkField::Address,
    }));
    let launch = WindowLaunch {
        snapshot: Arc::clone(&snapshot),
        commands: commands.clone(),
        keys: Arc::clone(&keys),
        locked_speed,
        turbo_ratio,
        turbo_held: Arc::clone(&turbo_held),
        library_mode: Arc::clone(&library_mode),
        controls: Arc::clone(&controls),
        link_ui: Arc::clone(&link_ui),
        windows_alive: Arc::new(AtomicUsize::new(0)),
        picture_windows: Arc::new(Mutex::new(PictureWindows {
            next_token: 0,
            #[cfg(target_os = "macos")]
            metal: None,
            #[cfg(target_os = "macos")]
            sharpen: None,
            cpu: None,
            linear: None,
        })),
        chrome_dirty: Arc::new(AtomicBool::new(false)),
    };
    let menu_commands = commands.clone();
    let menu_library_mode = Arc::clone(&library_mode);
    let menu_snapshot = Arc::clone(&snapshot);
    Application::new().run(move |cx: &mut App| {
        let (display_w, display_h) = display_point_size(cx);
        let main_mode = main_picture();
        let picture_token = launch
            .picture_windows
            .lock()
            .expect("picture windows poisoned")
            .alloc_token();
        let main_bounds = if compare_on_launch {
            layout_frames(picture_modes().len(), display_w, display_h)[0]
        } else {
            Bounds::centered(None, size(px(1040.), px(720.)), cx)
        };
        let main_window = open_game_window(
            cx,
            main_bounds,
            "ULTIMA",
            true,
            main_mode,
            false,
            picture_token,
            launch.clone(),
        );
        launch
            .picture_windows
            .lock()
            .expect("picture windows poisoned")
            .insert(
                main_mode,
                PictureSlot {
                    token: picture_token,
                    handle: main_window,
                },
            );
        if compare_on_launch {
            open_screen_comparison(cx, &launch);
        }
        let launch_metal = launch.clone();
        #[cfg(target_os = "macos")]
        cx.on_action(move |_: &ToggleMetalScreen, cx| {
            toggle_picture(cx, PictureMode::Metal, &launch_metal);
        });
        let launch_sharpen = launch.clone();
        #[cfg(target_os = "macos")]
        cx.on_action(move |_: &ToggleSharpenScreen, cx| {
            toggle_picture(cx, PictureMode::Sharpen, &launch_sharpen);
        });
        let launch_cpu = launch.clone();
        cx.on_action(move |_: &ToggleCpuScreen, cx| {
            toggle_picture(cx, PictureMode::CpuNearest, &launch_cpu);
        });
        let launch_linear = launch.clone();
        cx.on_action(move |_: &ToggleLinearScreen, cx| {
            toggle_picture(cx, PictureMode::Linear, &launch_linear);
        });
        let launch_all = launch.clone();
        cx.on_action(move |_: &CompareScreens, cx| {
            open_screen_comparison(cx, &launch_all);
        });
        if !compare_on_launch {
            let (speed, turbo) = {
                let settings = launch.controls.lock().expect("control settings poisoned");
                (settings.speed, settings.turbo)
            };
            cx.set_menus(app_menus(&[main_mode], speed, turbo));
        }
        let speed_launch = launch.clone();
        cx.on_action(move |action: &SetLockedSpeed, cx| {
            apply_speed_choice(&speed_launch, Some(action.0), None);
            refresh_picture_chrome(cx, &speed_launch.picture_windows, &speed_launch.controls);
        });
        let turbo_launch = launch.clone();
        cx.on_action(move |action: &SetTurboSpeed, cx| {
            apply_speed_choice(&turbo_launch, None, Some(action.0));
            refresh_picture_chrome(cx, &turbo_launch.picture_windows, &turbo_launch.controls);
        });
        cx.bind_keys([
            KeyBinding::new("cmd-o", OpenRom, None),
            KeyBinding::new("cmd-shift-o", ShowLibrary, None),
            KeyBinding::new("cmd-s", SaveSlot1, None),
            KeyBinding::new("space", TogglePause, None),
        ]);
        let open_commands = menu_commands.clone();
        let open_library_mode = Arc::clone(&library_mode);
        cx.on_action(move |_: &OpenRom, cx| {
            choose_rom_from_menu(cx, open_commands.clone(), Arc::clone(&open_library_mode));
        });
        let cover_commands = menu_commands.clone();
        cx.on_action(move |_: &ChooseCover, cx| {
            choose_cover_from_menu(cx, cover_commands.clone(), Arc::clone(&menu_snapshot));
        });
        cx.on_action(move |_: &ShowLibrary, cx| {
            menu_library_mode.store(true, Ordering::Relaxed);
            cx.refresh_windows();
        });
        let menu_library_mode = Arc::clone(&library_mode);
        cx.on_action(move |_: &ShowGame, cx| {
            menu_library_mode.store(false, Ordering::Relaxed);
            cx.refresh_windows();
        });
        let pause_commands = menu_commands.clone();
        cx.on_action(move |_: &TogglePause, _| {
            let _ = pause_commands.send(Command::TogglePause);
        });
        let reset_commands = menu_commands.clone();
        cx.on_action(move |_: &ResetConsole, _| {
            let _ = reset_commands.send(Command::Reset);
        });
        let show_controls = Arc::clone(&controls);
        let show_keys = Arc::clone(&keys);
        let show_turbo = Arc::clone(&turbo_held);
        let show_link = Arc::clone(&link_ui);
        cx.on_action(move |_: &ShowControls, cx| {
            let mut settings = show_controls.lock().expect("control settings poisoned");
            settings.open = true;
            settings.capturing = None;
            drop(settings);
            show_link.lock().expect("link ui poisoned").open = false;
            show_keys.store(0, Ordering::Relaxed);
            show_turbo.store(false, Ordering::Relaxed);
            cx.refresh_windows();
        });
        let open_link = Arc::clone(&link_ui);
        let open_controls = Arc::clone(&controls);
        let open_keys = Arc::clone(&keys);
        let open_turbo = Arc::clone(&turbo_held);
        cx.on_action(move |_: &ShowLink, cx| {
            let mut settings = open_controls.lock().expect("control settings poisoned");
            settings.open = false;
            settings.capturing = None;
            drop(settings);
            open_link.lock().expect("link ui poisoned").open = true;
            open_keys.store(0, Ordering::Relaxed);
            open_turbo.store(false, Ordering::Relaxed);
            cx.refresh_windows();
        });
        let bios_commands = menu_commands.clone();
        cx.on_action(move |_: &ToggleBiosHle, _| {
            let _ = bios_commands.send(Command::ToggleBiosHle);
        });
        let save_commands = menu_commands.clone();
        cx.on_action(move |_: &SaveSlot1, _| {
            let _ = save_commands.send(Command::SaveStateSlot(1));
        });
        let save_commands = menu_commands.clone();
        cx.on_action(move |_: &SaveSlot2, _| {
            let _ = save_commands.send(Command::SaveStateSlot(2));
        });
        let save_commands = menu_commands.clone();
        cx.on_action(move |_: &SaveSlot3, _| {
            let _ = save_commands.send(Command::SaveStateSlot(3));
        });
        let load_commands = menu_commands.clone();
        cx.on_action(move |_: &LoadSlot1, _| {
            let _ = load_commands.send(Command::LoadStateSlot(1));
        });
        let load_commands = menu_commands.clone();
        cx.on_action(move |_: &LoadSlot2, _| {
            let _ = load_commands.send(Command::LoadStateSlot(2));
        });
        let load_commands = menu_commands.clone();
        cx.on_action(move |_: &LoadSlot3, _| {
            let _ = load_commands.send(Command::LoadStateSlot(3));
        });
        let save_commands = menu_commands.clone();
        cx.on_action(move |_: &QuickSaveState, _| {
            let _ = save_commands.send(Command::QuickSaveState);
        });
        let load_commands = menu_commands.clone();
        cx.on_action(move |_: &QuickLoadState, _| {
            let _ = load_commands.send(Command::QuickLoadState);
        });
        let volume_up = Arc::clone(&audio_volume);
        cx.on_action(move |_: &VolumeUp, _| {
            let volume = volume_up
                .load(Ordering::Relaxed)
                .saturating_add(10)
                .min(100);
            volume_up.store(volume, Ordering::Relaxed);
        });
        let volume_down = Arc::clone(&audio_volume);
        cx.on_action(move |_: &VolumeDown, _| {
            let volume = volume_down.load(Ordering::Relaxed).saturating_sub(10);
            volume_down.store(volume, Ordering::Relaxed);
        });
        let volume_mute = Arc::clone(&audio_volume);
        cx.on_action(move |_: &ToggleMute, _| {
            let volume = volume_mute.load(Ordering::Relaxed);
            volume_mute.store(if volume == 0 { 70 } else { 0 }, Ordering::Relaxed);
        });
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
    let _ = commands.send(Command::Shutdown);
    if worker.join().is_err() {
        eprintln!("模拟器线程异常退出");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_boot_animation_progress_stops_after_its_intro() {
        let started_at = Instant::now();
        assert_eq!(
            boot_animation_duration(),
            BOOT_ANIMATION_REVEAL_DURATION + BOOT_ANIMATION_HOLD_DURATION
        );
        assert_eq!(boot_progress(Some(started_at), started_at), Some(0.0));
        let revealed = boot_progress(
            Some(started_at),
            started_at + BOOT_ANIMATION_REVEAL_DURATION,
        )
        .expect("settled frame is still on screen");
        assert!((revealed - 1.0).abs() < 0.001);
        let holding = boot_progress(
            Some(started_at),
            started_at + BOOT_ANIMATION_REVEAL_DURATION + BOOT_ANIMATION_HOLD_DURATION / 2,
        )
        .expect("final frame stays up during the hold");
        assert!((holding - 1.0).abs() < 0.001);
        assert_eq!(
            boot_progress(Some(started_at), started_at + boot_animation_duration()),
            None
        );
        assert_eq!(boot_progress(None, started_at), None);
    }

    #[test]
    fn comparison_tiles_fit_without_overlapping() {
        for (width, height) in [(1512., 982.), (1728., 1117.), (1440., 900.), (1280., 800.)] {
            let tiles = comparison_tiles(width, height);
            for index in 0..tiles.len() {
                let (x, y, tile_w, tile_h) = tiles[index];
                assert!(x >= 0.0 && y >= 0.0, "{width}x{height} origin");
                assert!(x + tile_w <= width + 0.5, "{width}x{height} width");
                assert!(y + tile_h <= height + 0.5, "{width}x{height} height");
                for other in (index + 1)..tiles.len() {
                    let (ox, oy, ow, oh) = tiles[other];
                    let separated = x + tile_w <= ox + 0.5
                        || ox + ow <= x + 0.5
                        || y + tile_h <= oy + 0.5
                        || oy + oh <= y + 0.5;
                    assert!(separated, "{width}x{height} tiles overlap");
                }
            }
        }
        let wide = comparison_tiles(1512., 982.);
        assert_eq!(wide[0].1, wide[2].1);
        let narrow = comparison_tiles(1280., 800.);
        assert!(narrow[2].1 > narrow[0].1);
        let pair = layout_tiles(2, 1512., 982.);
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0].1, pair[1].1);
        assert!(pair[0].0 + pair[0].2 <= pair[1].0 + 0.5);
        let quad = layout_tiles(4, 1512., 982.);
        assert_eq!(quad.len(), 4);
        assert!(quad[2].1 > quad[0].1);
        let wide_quad = layout_tiles(4, 2560., 1440.);
        assert_eq!(wide_quad.len(), 4);
        assert_eq!(wide_quad[0].1, wide_quad[3].1);
    }

    #[test]
    fn picture_options_toggle_and_keep_the_last_window() {
        assert_eq!(picture_toggle(1, true), PictureToggle::Focus);
        assert_eq!(picture_toggle(2, true), PictureToggle::Close);
        assert_eq!(picture_toggle(1, false), PictureToggle::Switch);
        assert_eq!(picture_toggle(2, false), PictureToggle::Open);
        assert_eq!(picture_toggle(0, false), PictureToggle::Open);
        assert_eq!(
            picture_option_label("Metal 最近邻", true),
            "✓ Metal 最近邻"
        );
        assert_eq!(picture_option_label("线性放大", false), "线性放大");
        let orders: Vec<_> = picture_modes().into_iter().map(|mode| mode.order()).collect();
        let mut sorted = orders.clone();
        sorted.sort_unstable();
        assert_eq!(orders, sorted);
    }

    #[test]
    fn picture_modes_have_distinct_names() {
        let mut names = vec![
            PictureMode::CpuNearest.title(),
            PictureMode::Linear.title(),
        ];
        #[cfg(target_os = "macos")]
        names.push(PictureMode::Metal.title());
        names.push(PictureMode::Sharpen.title());
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        names.sort_unstable();
        assert_eq!(names, unique);
        assert!(names.iter().all(|name| !name.is_empty()));
    }

    #[test]
    fn custom_boot_animation_easing_and_colors_have_stable_endpoints() {
        assert_eq!(boot_ease_out(0.0), 0.0);
        assert_eq!(boot_ease_out(1.0), 1.0);
        assert_eq!(mix_color(0x000000, 0xFFFFFF, 0.0), 0x000000);
        assert_eq!(mix_color(0x000000, 0xFFFFFF, 0.5), 0x808080);
        assert_eq!(mix_color(0x000000, 0xFFFFFF, 1.0), 0xFFFFFF);
    }

    #[test]
    fn keymap_art_is_a_landscape_gba() {
        let image = image::load_from_memory(keymap_png_bytes()).expect("decode keymap art");
        assert_eq!(image.width(), KEYMAP_WIDTH as u32);
        assert_eq!(image.height(), KEYMAP_HEIGHT as u32);
        assert!(image.width() > image.height());
    }

    #[test]
    fn converts_a_full_gba_frame_into_rgba_pixels() {
        let bus = Bus::new();
        let rgba = frame_to_rgba(bus.frame());
        assert_eq!(rgba.len(), WIDTH * HEIGHT * 4);
        assert_eq!(rgba[3], 0xFF);
        assert_eq!(rgba[rgba.len() - 1], 0xFF);
    }

    #[test]
    fn converts_gba_red_pixels_to_red_rgba_output() {
        let mut bus = Bus::new();
        bus.write_u16(0x0400_0000, 0x0403);
        bus.write_u16(0x0600_0000, 0x001F);
        let frame = gameboy_ultima::render_frame(&mut bus);
        let rgba = frame_to_rgba(&frame);
        assert_eq!(&rgba[..4], &[0xFF, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn converts_rgba_pixels_to_the_bgra_order_used_by_gpui() {
        let rgba = ImageBuffer::from_pixel(1, 1, Rgba([255, 96, 24, 255]));
        let bgra = rgba_buffer_to_bgra(rgba);
        assert_eq!(bgra.as_raw(), &[24, 96, 255, 255]);
    }

    #[test]
    fn reads_title_and_game_code_from_the_gba_header() {
        let mut rom = vec![0; 0xB0];
        rom[0xA0..0xA0 + 8].copy_from_slice(b"TESTGAME");
        rom[0xAC..0xB0].copy_from_slice(b"ABCD");
        assert_eq!(
            rom_metadata(&rom, Path::new("fallback.gba")),
            ("TESTGAME".into(), "ABCD".into())
        );
    }

    #[test]
    fn only_accepts_gba_rom_extensions_case_insensitively() {
        assert!(is_gba_rom(Path::new("game.gba")));
        assert!(is_gba_rom(Path::new("game.GBA")));
        assert!(!is_gba_rom(Path::new("game.zip")));
        assert!(!is_gba_rom(Path::new("game")));
    }

    #[test]
    fn maps_save_slots_to_namespaced_state_files() {
        let directory = Path::new("/app/save_states/rom-hash");
        assert_eq!(
            state_slot_path_in(directory, 1).unwrap(),
            directory.join("slot-1.state")
        );
        assert_eq!(
            state_slot_path_in(directory, 2).unwrap(),
            directory.join("slot-2.state")
        );
        assert_eq!(
            state_slot_path_in(directory, 3).unwrap(),
            directory.join("slot-3.state")
        );
        assert_eq!(
            quick_state_path_in(directory),
            directory.join("quick.state")
        );
        assert!(state_slot_path_in(directory, 4).is_err());
    }

    #[test]
    fn identifies_roms_by_content() {
        let first = [0x01, 0x02, 0x03, 0x04];
        let identical = [0x01, 0x02, 0x03, 0x04];
        let different = [0x01, 0x02, 0x03, 0x05];
        assert_eq!(rom_identity(&first), rom_identity(&identical));
        assert_ne!(rom_identity(&first), rom_identity(&different));
    }

    #[test]
    fn finds_the_most_recent_available_save_for_quick_load() {
        let directory = std::env::temp_dir().join(format!(
            "gameboy-ultima-save-slots-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("create save-slot test directory");
        assert_eq!(latest_state_path_in(&directory).unwrap(), None);

        let quick_path = quick_state_path_in(&directory);
        std::fs::write(&quick_path, b"state").expect("write quick save fixture");
        assert_eq!(latest_state_path_in(&directory).unwrap(), Some(quick_path));
        std::fs::remove_dir_all(directory).expect("remove save-slot test directory");
    }

    #[test]
    fn keeps_a_generated_screenshot_when_the_library_is_reread() {
        let directory = std::env::temp_dir().join(format!(
            "gameboy-ultima-generated-cover-test-{}",
            std::process::id()
        ));
        let path = directory.join("library.json");
        let entries = vec![LibraryEntry {
            path: PathBuf::from("/games/test.gba"),
            title: "Test Game".into(),
            game_code: "ABCD".into(),
            cover_path: Some(PathBuf::from("/covers/abc-generated.png")),
        }];

        write_library(&path, &entries).expect("write library");
        let restored = read_library(&path).expect("read library");
        assert_eq!(restored[0].cover_path, entries[0].cover_path);
        std::fs::remove_dir_all(directory).expect("remove library test directory");
    }

    #[test]
    fn integer_screen_scale_picks_the_largest_multiple_that_fits() {
        assert_eq!(integer_screen_scale(1040.0, 720.0, 2.0), 8);
        assert_eq!(integer_screen_scale(1040.0, 720.0, 1.0), 4);
        assert_eq!(integer_screen_scale(100.0, 80.0, 1.0), 1);
        assert_eq!(integer_screen_scale(1040.0, 720.0, 0.0), 4);
        let factor = integer_screen_scale(1040.0, 720.0, 2.0);
        let (width, height) = screen_point_size(factor, 2.0);
        assert_eq!((width, height), (960.0, 640.0));
        assert!(width <= 1040.0 && height <= 720.0);
        let fullscreen = integer_screen_scale(1512.0, 982.0, 2.0);
        assert!(fullscreen > factor);
        let (full_w, full_h) = screen_point_size(fullscreen, 2.0);
        assert!(full_w <= 1512.0 && full_h <= 982.0);
    }

    #[test]
    fn fullscreen_viewport_replaces_a_stale_windowed_size() {
        assert_eq!(
            resolve_picture_viewport(1040.0, 720.0, Some((1512.0, 982.0)), Some((1512.0, 982.0))),
            (1512.0, 982.0)
        );
        assert_eq!(
            resolve_picture_viewport(1040.0, 720.0, None, Some((1512.0, 1010.0))),
            (1512.0, 1010.0)
        );
        assert_eq!(
            resolve_picture_viewport(1040.0, 720.0, Some((1040.0, 720.0)), Some((1040.0, 748.0))),
            (1040.0, 720.0)
        );
        assert_eq!(
            resolve_picture_viewport(1040.0, 720.0, Some((0.0, 10.0)), None),
            (1040.0, 720.0)
        );
    }

    #[test]
    fn nearest_scale_keeps_each_pixel_a_solid_block() {
        let mut frame = vec![0u8; WIDTH * HEIGHT * 4];
        frame[..4].copy_from_slice(&[0x11, 0x22, 0x33, 0xFF]);
        frame[4..8].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xFF]);
        let image = scale_gba_nearest_rgba(&frame, 2).expect("scale the frame");
        assert_eq!(
            image.dimensions(),
            ((WIDTH * 2) as u32, (HEIGHT * 2) as u32)
        );
        assert_eq!(image.get_pixel(0, 0).0, [0x11, 0x22, 0x33, 0xFF]);
        assert_eq!(image.get_pixel(1, 1).0, [0x11, 0x22, 0x33, 0xFF]);
        assert_eq!(image.get_pixel(2, 0).0, [0xAA, 0xBB, 0xCC, 0xFF]);
        assert_eq!(image.get_pixel(3, 1).0, [0xAA, 0xBB, 0xCC, 0xFF]);
        assert!(scale_gba_nearest_rgba(&frame[..4], 2).is_none());
    }

    #[test]
    fn screenshot_cards_place_the_game_frame_under_the_accent_line() {
        let mut frame = vec![0u8; WIDTH * HEIGHT * 4];
        frame[..4].copy_from_slice(&[0xFF, 0x00, 0x00, 0xFF]);
        let image = compose_screenshot_cover(&frame);
        assert_eq!(image.dimensions(), (COVER_WIDTH, COVER_HEIGHT));
        assert_eq!(image.get_pixel(0, 0).0, [0x0B, 0x10, 0x12, 0xFF]);
        assert_eq!(
            image.get_pixel(0, COVER_ACCENT_TOP).0,
            [0xC8, 0xF1, 0x69, 0xFF]
        );
        assert_eq!(
            image.get_pixel(0, COVER_SCREEN_TOP).0,
            [0xFF, 0x00, 0x00, 0xFF]
        );
        assert_eq!(
            image.get_pixel(1, COVER_SCREEN_TOP + 1).0,
            [0xFF, 0x00, 0x00, 0xFF]
        );
    }

    #[test]
    fn custom_covers_are_not_replaced_by_a_screenshot() {
        assert!(is_custom_cover(Path::new("/covers/abc-custom.png")));
        assert!(!is_custom_cover(Path::new("/covers/abc-generated.png")));
        assert_eq!(
            screenshot_frame_delay(Some(Path::new("/covers/abc-custom.png"))),
            None
        );
        assert_eq!(screenshot_frame_delay(None), Some(SCREENSHOT_FRAME_DELAY));
    }

    #[test]
    fn persists_and_reads_the_cartridge_library() {
        let directory = std::env::temp_dir().join(format!(
            "gameboy-ultima-library-test-{}",
            std::process::id()
        ));
        let path = directory.join("library.json");
        let entries = vec![LibraryEntry {
            path: PathBuf::from("/games/test.gba"),
            title: "Test Game".into(),
            game_code: "ABCD".into(),
            cover_path: Some(PathBuf::from("/covers/test.png")),
        }];

        write_library(&path, &entries).expect("write library");

        let restored = read_library(&path).expect("read library");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].title, "Test Game");
        assert_eq!(restored[0].cover_path, entries[0].cover_path);
        std::fs::remove_dir_all(directory).expect("remove library test directory");
    }

    #[test]
    fn quick_state_restores_cpu_and_bus_and_rejects_another_rom() {
        thread::Builder::new()
            .name("quick-state-test".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(quick_state_round_trip)
            .expect("start quick-state test")
            .join()
            .expect("quick-state test thread");
    }

    fn quick_state_round_trip() {
        let directory =
            std::env::temp_dir().join(format!("gameboy-ultima-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create state test directory");
        let rom_path = directory.join("game.gba");
        let path = directory.join("slot-1.state");
        let temp_path = path.with_extension("state.tmp");

        let mut bus = Bus::new();
        let rom = [0x01, 0x02, 0x03, 0x04];
        bus.load_rom(&rom);
        let rom_id = rom_identity(&rom);
        bus.write_u32(0x0200_0000, 0x1234_5678);
        bus.set_bios_hle(false);
        let mut regs = Registers::arm_start(0x0800_0000);
        regs.set(0, 0xCAFE_BABE);
        let cart = ActiveCart {
            path: rom_path,
            rom_id,
            title: "Test".into(),
            game_code: "TEST".into(),
            cover_path: None,
            regs,
            bus,
        };

        write_state_to(&cart.rom_id, &cart.path, &cart.regs, &cart.bus, 42, &path)
            .expect("write quick state");

        let mut changed = ActiveCart {
            path: cart.path.clone(),
            rom_id: cart.rom_id.clone(),
            title: cart.title.clone(),
            game_code: cart.game_code.clone(),
            cover_path: cart.cover_path.clone(),
            regs: Registers::arm_start(0),
            bus: cart.bus.clone(),
        };
        changed.regs.set(0, 0);
        changed.bus.write_u32(0x0200_0000, 0);
        let (restored_regs, restored_bus, frame_number) =
            read_state(&path, &changed).expect("read quick state");
        assert_eq!(restored_regs.get(0), 0xCAFE_BABE);
        assert_eq!(restored_bus.read_u32(0x0200_0000), 0x1234_5678);
        assert!(!restored_bus.bios_hle());
        assert_eq!(frame_number, 42);

        changed.rom_id = rom_identity(&[0xFF]);
        assert!(read_state(&path, &changed).is_err());
        changed.rom_id = cart.rom_id.clone();
        changed.bus.load_rom(&[0xFF]);
        assert!(read_state(&path, &changed).is_err());
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(temp_path);
        let _ = std::fs::remove_dir(directory);
    }

    #[test]
    fn bios_hle_fast_path_is_enabled_by_default_and_can_be_toggled() {
        let mut bus = Bus::new();
        assert!(bus.bios_hle());
        bus.set_bios_hle(false);
        assert!(!bus.bios_hle());
    }
}
