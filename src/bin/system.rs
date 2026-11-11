use std::time::Instant;

use gpui::{div, prelude::*, px, rgb, rgba, Context, MouseButton, Window};
use super::{
    AppView, SetLockedSpeed, ToggleColorCorrect, ToggleLcdGhost, ToggleRumbleShake, SPEEDS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SystemRun {
    pub open: bool,
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemInput {
    Open,
    Resume,
    Hide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum SystemField {
    None,
    LinkName,
    LinkAddress,
    CheatName,
    CheatCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemKey {
    Resume,
    Edit,
    Ignore,
}

pub(crate) fn apply_system(state: SystemRun, has_cart: bool, input: SystemInput) -> SystemRun {
    if !has_cart {
        return state;
    }
    match input {
        SystemInput::Open => SystemRun { open: true, paused: true },
        SystemInput::Resume => SystemRun { open: false, paused: false },
        SystemInput::Hide => SystemRun { open: false, paused: state.paused },
    }
}

pub(crate) fn system_key(field: SystemField, key: &str, system_binding: &str) -> SystemKey {
    if key == system_binding {
        return if field == SystemField::None {
            SystemKey::Resume
        } else {
            SystemKey::Ignore
        };
    }
    if field == SystemField::None {
        SystemKey::Ignore
    } else {
        SystemKey::Edit
    }
}

pub(crate) fn system_stops_link(_input: SystemInput) -> bool {
    false
}

#[cfg(test)]
pub(crate) fn system_link_highlight(field: SystemField) -> Option<bool> {
    match field {
        SystemField::LinkName => Some(true),
        SystemField::LinkAddress => Some(false),
        SystemField::None | SystemField::CheatName | SystemField::CheatCode => None,
    }
}

#[cfg(test)]
pub(crate) fn system_cheat_highlight(field: SystemField) -> Option<bool> {
    match field {
        SystemField::CheatName => Some(true),
        SystemField::CheatCode => Some(false),
        SystemField::None | SystemField::LinkName | SystemField::LinkAddress => None,
    }
}

pub(crate) fn system_typing_binding() -> gpui::KeyBinding {
    gpui::KeyBinding::new("space", gpui::NoAction, Some("SystemTyping"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugBack {
    Field,
    Menu,
    Stay,
}

pub(crate) fn debug_back(editing: bool, key: &str, system_binding: &str) -> DebugBack {
    if key == "escape" {
        if editing {
            DebugBack::Field
        } else {
            DebugBack::Menu
        }
    } else if key == system_binding && !editing {
        DebugBack::Menu
    } else {
        DebugBack::Stay
    }
}

pub(crate) fn pause_key_while_open(open: bool) -> Option<SystemInput> {
    if open {
        Some(SystemInput::Resume)
    } else {
        None
    }
}

pub(crate) const PIXEL_STEP_MS: u128 = 50;

pub(crate) const PIXEL_INK: u32 = 0x181818;
pub(crate) const PIXEL_PAPER: u32 = 0xF4F0E0;
pub(crate) const PIXEL_LIGHT: u32 = 0xFFFFFF;
pub(crate) const PIXEL_SHADE: u32 = 0x6E6A60;
pub(crate) const PIXEL_QUIET: u32 = 0x5E5A50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PixelHit {
    Resume,
    Speed(u8),
    Ghost,
    Color,
    Rumble,
    Debug,
    Cheats,
    Link,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PixelFace {
    pub light: u32,
    pub shade: u32,
    pub fill: u32,
    pub ink: u32,
}

pub(crate) fn pixel_reveal(elapsed_ms: u128, closing: bool) -> u8 {
    let slot = (elapsed_ms / PIXEL_STEP_MS).min(3) as u8;
    if closing { 3 - slot } else { slot + 1 }
}

pub(crate) fn close_rewind_ms(shown: u8) -> u128 {
    PIXEL_STEP_MS * u128::from(4 - shown.clamp(1, 4))
}

pub(crate) fn pixel_face(selected: bool, pressed: bool) -> PixelFace {
    let (light, shade) = if pressed {
        (PIXEL_SHADE, PIXEL_LIGHT)
    } else {
        (PIXEL_LIGHT, PIXEL_SHADE)
    };
    if selected {
        PixelFace {
            light,
            shade,
            fill: PIXEL_INK,
            ink: PIXEL_PAPER,
        }
    } else {
        PixelFace {
            light,
            shade,
            fill: PIXEL_PAPER,
            ink: PIXEL_INK,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PopMotion {
    armed: bool,
    closing: bool,
}

impl PopMotion {
    fn idle() -> Self {
        Self {
            armed: false,
            closing: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PopTick {
    pub step: u8,
    pub dismiss: bool,
}

pub(crate) struct PopClock {
    motion: PopMotion,
    since: Instant,
}

impl PopClock {
    pub(crate) fn new() -> Self {
        Self {
            motion: PopMotion::idle(),
            since: Instant::now(),
        }
    }

    pub(crate) fn arm(&mut self, now: Instant) {
        self.motion.armed = true;
        self.motion.closing = false;
        self.since = now;
    }

    pub(crate) fn request_close(&mut self, now: Instant) {
        if self.motion.armed && !self.motion.closing {
            self.motion.closing = true;
            self.since = now;
        }
    }

    pub(crate) fn advance(&mut self, open: bool, now: Instant) -> PopTick {
        if !open {
            self.motion = PopMotion::idle();
            return PopTick {
                step: 0,
                dismiss: false,
            };
        }
        if !self.motion.armed {
            self.arm(now);
        }
        let elapsed = now.saturating_duration_since(self.since).as_millis();
        let step = pixel_reveal(elapsed, self.motion.closing);
        if self.motion.closing && step == 0 {
            self.motion = PopMotion::idle();
            return PopTick {
                step: 0,
                dismiss: true,
            };
        }
        PopTick {
            step,
            dismiss: false,
        }
    }
}

pub(crate) fn system_page(
    cx: &mut Context<AppView>,
    title: &str,
    locked_speed: u8,
    lcd_ghost: bool,
    color_correct: bool,
    rumble_shake: bool,
    step: u8,
    hover: Option<PixelHit>,
    pressed: Option<PixelHit>,
) -> impl IntoElement {
    let lift = px((4 - step.min(4)) as f32 * 2.);
    div()
        .id("system-page")
        .absolute()
        .top(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .left(px(0.))
        .opacity(step as f32 / 4.)
        .child(
            div()
                .absolute()
                .top(px(0.))
                .right(px(0.))
                .bottom(px(0.))
                .left(px(0.))
                .bg(rgba(0x0000_0099)),
        )
        .child(
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div().mt(lift).child(pixel_shell(
                        pixel_face(false, false),
                        div()
                            .w(px(320.))
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .p(px(14.))
                            .child(div().text_lg().child("系统"))
                            .when(!title.is_empty(), |page| {
                                page.child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(PIXEL_QUIET))
                                        .child(title.to_string()),
                                )
                            })
                            .child(pixel_button(
                                cx,
                                "system-resume",
                                "恢复",
                                PixelHit::Resume,
                                false,
                                hover,
                                pressed,
                                true,
                            ))
                            .child(div().text_xs().text_color(rgb(PIXEL_QUIET)).child("速度"))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(8.))
                                    .children(SPEEDS.map(|speed| {
                                        pixel_button(
                                            cx,
                                            ("system-speed", speed as usize),
                                            format!("{speed}×"),
                                            PixelHit::Speed(speed),
                                            speed == locked_speed,
                                            hover,
                                            pressed,
                                            false,
                                        )
                                    })),
                            )
                            .child(div().text_xs().text_color(rgb(PIXEL_QUIET)).child("画面"))
                            .child(pixel_switch_row(
                                cx,
                                "system-ghost",
                                "液晶余辉",
                                PixelHit::Ghost,
                                lcd_ghost,
                                hover,
                                pressed,
                            ))
                            .child(pixel_switch_row(
                                cx,
                                "system-color",
                                "色彩校正",
                                PixelHit::Color,
                                color_correct,
                                hover,
                                pressed,
                            ))
                            .child(pixel_switch_row(
                                cx,
                                "system-rumble",
                                "画面震动",
                                PixelHit::Rumble,
                                rumble_shake,
                                hover,
                                pressed,
                            ))
                            .child(pixel_button(
                                cx,
                                "system-debug",
                                "调试",
                                PixelHit::Debug,
                                false,
                                hover,
                                pressed,
                                true,
                            ))
                            .child(pixel_button(
                                cx,
                                "system-cheats",
                                "金手指",
                                PixelHit::Cheats,
                                false,
                                hover,
                                pressed,
                                true,
                            ))
                            .child(pixel_button(
                                cx,
                                "system-link",
                                "联机",
                                PixelHit::Link,
                                false,
                                hover,
                                pressed,
                                true,
                            )),
                    )),
                ),
        )
}

pub(crate) fn pixel_shell(face: PixelFace, child: impl IntoElement) -> impl IntoElement {
    div().bg(rgb(PIXEL_INK)).p(px(2.)).child(
        div().bg(rgb(face.shade)).pr(px(2.)).pb(px(2.)).child(
            div().bg(rgb(face.light)).pt(px(2.)).pl(px(2.)).child(
                div()
                    .bg(rgb(face.fill))
                    .text_color(rgb(face.ink))
                    .child(child),
            ),
        ),
    )
}

pub(crate) fn pixel_arrow(ink: u32) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(pixel_mark(2., 4., ink))
        .child(pixel_mark(4., 2., ink))
        .child(pixel_mark(6., 0., ink))
        .child(pixel_mark(4., 2., ink))
        .child(pixel_mark(2., 4., ink))
}

fn pixel_mark(width: f32, indent: f32, ink: u32) -> impl IntoElement {
    div()
        .ml(px(indent))
        .w(px(width))
        .h(px(2.))
        .bg(rgb(ink))
}

pub(crate) fn pixel_switch(on: bool) -> impl IntoElement {
    div().bg(rgb(PIXEL_INK)).p(px(2.)).child(
        div()
            .flex()
            .bg(rgb(PIXEL_PAPER))
            .w(px(28.))
            .h(px(14.))
            .child(div().w(px(14.)).h(px(14.)).bg(rgb(if on { PIXEL_PAPER } else { PIXEL_INK })))
            .child(div().w(px(14.)).h(px(14.)).bg(rgb(if on { PIXEL_INK } else { PIXEL_PAPER }))),
    )
}

fn pixel_button(
    cx: &mut Context<AppView>,
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    hit: PixelHit,
    selected: bool,
    hover: Option<PixelHit>,
    pressed: Option<PixelHit>,
    wide: bool,
) -> impl IntoElement {
    let label = label.into();
    let down = pressed == Some(hit);
    let face = pixel_face(selected, down);
    let show_arrow = hover == Some(hit);
    div()
        .id(id)
        .cursor_pointer()
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            let mut chrome = this.system_chrome.lock().expect("system chrome poisoned");
            if *hovered {
                chrome.hover = Some(hit);
            } else if chrome.hover == Some(hit) {
                chrome.hover = None;
            }
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.system_chrome.lock().expect("system chrome poisoned").pressed = Some(hit);
                cx.notify();
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                let was = this
                    .system_chrome
                    .lock()
                    .expect("system chrome poisoned")
                    .pressed
                    .take();
                if was == Some(hit) {
                    activate_pixel(this, hit, window, cx);
                }
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                let mut chrome = this.system_chrome.lock().expect("system chrome poisoned");
                if chrome.pressed == Some(hit) {
                    chrome.pressed = None;
                }
                cx.notify();
            }),
        )
        .child(pixel_shell(
            face,
            div()
                .when(wide, |row| row.w(px(292.)))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .px(px(12.))
                .py(px(8.))
                .child(div().w(px(8.)).when(show_arrow, |slot| slot.child(pixel_arrow(face.ink))))
                .child(label)
                .child(div().w(px(8.))),
        ))
}

fn pixel_switch_row(
    cx: &mut Context<AppView>,
    id: &'static str,
    label: &'static str,
    hit: PixelHit,
    on: bool,
    hover: Option<PixelHit>,
    pressed: Option<PixelHit>,
) -> impl IntoElement {
    let down = pressed == Some(hit);
    let face = pixel_face(false, down);
    let show_arrow = hover == Some(hit);
    div()
        .id(id)
        .cursor_pointer()
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            let mut chrome = this.system_chrome.lock().expect("system chrome poisoned");
            if *hovered {
                chrome.hover = Some(hit);
            } else if chrome.hover == Some(hit) {
                chrome.hover = None;
            }
            cx.notify();
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.system_chrome.lock().expect("system chrome poisoned").pressed = Some(hit);
                cx.notify();
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                let was = this
                    .system_chrome
                    .lock()
                    .expect("system chrome poisoned")
                    .pressed
                    .take();
                if was == Some(hit) {
                    activate_pixel(this, hit, window, cx);
                }
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                let mut chrome = this.system_chrome.lock().expect("system chrome poisoned");
                if chrome.pressed == Some(hit) {
                    chrome.pressed = None;
                }
                cx.notify();
            }),
        )
        .child(pixel_shell(
            face,
            div()
                .w(px(292.))
                .flex()
                .items_center()
                .justify_between()
                .px(px(12.))
                .py(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().w(px(8.)).when(show_arrow, |slot| slot.child(pixel_arrow(face.ink))))
                        .child(label),
                )
                .child(pixel_switch(on)),
        ))
}

fn activate_pixel(this: &mut AppView, hit: PixelHit, window: &mut Window, cx: &mut Context<AppView>) {
    this.system_field = SystemField::None;
    let now = Instant::now();
    match hit {
        PixelHit::Resume => this.begin_system_close(cx),
        PixelHit::Speed(speed) => window.dispatch_action(Box::new(SetLockedSpeed(speed)), cx),
        PixelHit::Ghost => window.dispatch_action(Box::new(ToggleLcdGhost), cx),
        PixelHit::Color => window.dispatch_action(Box::new(ToggleColorCorrect), cx),
        PixelHit::Rumble => window.dispatch_action(Box::new(ToggleRumbleShake), cx),
        PixelHit::Debug => {
            this.link_ui.lock().expect("link ui poisoned").open = false;
            this.cheat_ui.lock().expect("cheat ui poisoned").open = false;
            this.controls.lock().expect("control settings poisoned").open = false;
            this.debug_ui.open = true;
            this.debug_pop.arm(now);
            cx.notify();
        }
        PixelHit::Cheats => {
            this.link_ui.lock().expect("link ui poisoned").open = false;
            this.controls.lock().expect("control settings poisoned").open = false;
            this.debug_ui.open = false;
            {
                let mut cheat = this.cheat_ui.lock().expect("cheat ui poisoned");
                cheat.open = true;
                cheat.editing = false;
                cheat.pressed = None;
            }
            this.cheat_pop.arm(now);
            cx.notify();
        }
        PixelHit::Link => {
            this.cheat_ui.lock().expect("cheat ui poisoned").open = false;
            this.controls.lock().expect("control settings poisoned").open = false;
            this.debug_ui.open = false;
            {
                let mut link = this.link_ui.lock().expect("link ui poisoned");
                link.open = true;
                link.editing = false;
                link.pressed = None;
            }
            this.link_pop.arm(now);
            cx.notify();
        }
    }
}

pub(crate) fn fade_shade(step: u8, child: impl IntoElement) -> impl IntoElement {
    div()
        .absolute()
        .top(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .left(px(0.))
        .opacity(step as f32 / 4.)
        .child(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_pauses_and_resume_continues() {
        let running = SystemRun { open: false, paused: false };
        let open = apply_system(running, true, SystemInput::Open);
        assert_eq!(open, SystemRun { open: true, paused: true });
        let again = apply_system(
            SystemRun { open: false, paused: true },
            true,
            SystemInput::Open,
        );
        assert_eq!(again, SystemRun { open: true, paused: true });
        assert_eq!(
            apply_system(open, true, SystemInput::Resume),
            SystemRun { open: false, paused: false }
        );
    }

    #[test]
    fn no_cart_and_hide_leave_the_pause_bit_alone() {
        let running = SystemRun { open: false, paused: false };
        assert_eq!(apply_system(running, false, SystemInput::Open), running);
        let suspended = SystemRun { open: true, paused: true };
        assert_eq!(
            apply_system(suspended, true, SystemInput::Hide),
            SystemRun { open: false, paused: true }
        );
    }

    #[test]
    fn f7_resumes_only_when_no_field_is_focused() {
        assert_eq!(system_key(SystemField::None, "f7", "f7"), SystemKey::Resume);
        assert_eq!(system_key(SystemField::LinkName, "f7", "f7"), SystemKey::Ignore);
        assert_eq!(system_key(SystemField::CheatCode, "a", "f7"), SystemKey::Edit);
        assert_eq!(system_key(SystemField::None, "a", "f7"), SystemKey::Ignore);
        assert!(!system_stops_link(SystemInput::Open));
        assert!(!system_stops_link(SystemInput::Resume));
        assert!(!system_stops_link(SystemInput::Hide));
        assert_eq!(pause_key_while_open(true), Some(SystemInput::Resume));
        assert_eq!(pause_key_while_open(false), None);
    }

    #[test]
    fn a_rebound_system_key_is_the_one_that_resumes() {
        assert_eq!(
            system_key(SystemField::None, "f9", "f9"),
            SystemKey::Resume
        );
        assert_eq!(
            system_key(SystemField::None, "f7", "f9"),
            SystemKey::Ignore
        );
        assert_eq!(
            system_key(SystemField::LinkName, "f9", "f9"),
            SystemKey::Ignore
        );
    }

    #[test]
    fn a_focused_system_field_types_space_instead_of_resuming() {
        let keymap = gpui::Keymap::new(vec![
            gpui::KeyBinding::new("space", super::super::TogglePause, None),
            system_typing_binding(),
        ]);
        let space = [gpui::Keystroke::parse("space").unwrap()];
        let typing = [gpui::KeyContext::parse("SystemTyping").unwrap()];
        let idle = [gpui::KeyContext::parse("System").unwrap()];
        let (typing_hit, _) = keymap.bindings_for_input(&space, &typing);
        let (idle_hit, _) = keymap.bindings_for_input(&space, &idle);
        assert!(typing_hit.is_empty());
        assert_eq!(idle_hit.len(), 1);
        assert_eq!(
            system_key(SystemField::CheatCode, "space", "f7"),
            SystemKey::Edit
        );
        assert_eq!(system_key(SystemField::LinkName, "space", "f7"), SystemKey::Edit);
        assert_eq!(system_key(SystemField::None, "space", "f7"), SystemKey::Ignore);
    }

    #[test]
    fn system_highlight_follows_the_focused_field_only() {
        assert_eq!(system_link_highlight(SystemField::None), None);
        assert_eq!(system_link_highlight(SystemField::LinkName), Some(true));
        assert_eq!(system_link_highlight(SystemField::LinkAddress), Some(false));
        assert_eq!(system_link_highlight(SystemField::CheatName), None);
        assert_eq!(system_cheat_highlight(SystemField::None), None);
        assert_eq!(system_cheat_highlight(SystemField::CheatName), Some(true));
        assert_eq!(system_cheat_highlight(SystemField::CheatCode), Some(false));
        assert_eq!(system_cheat_highlight(SystemField::LinkAddress), None);
    }

    #[test]
    fn the_menu_steps_in_and_back_out() {
        assert_eq!(pixel_reveal(0, false), 1);
        assert_eq!(pixel_reveal(49, false), 1);
        assert_eq!(pixel_reveal(50, false), 2);
        assert_eq!(pixel_reveal(100, false), 3);
        assert_eq!(pixel_reveal(150, false), 4);
        assert_eq!(pixel_reveal(400, false), 4);
        assert_eq!(pixel_reveal(0, true), 3);
        assert_eq!(pixel_reveal(50, true), 2);
        assert_eq!(pixel_reveal(100, true), 1);
        assert_eq!(pixel_reveal(150, true), 0);
        for shown in 1..=4 {
            assert_eq!(pixel_reveal(close_rewind_ms(shown), true), shown - 1);
        }
    }

    #[test]
    fn a_pressed_pixel_button_sinks_and_the_selected_one_inverts() {
        let raised = pixel_face(false, false);
        let sunk = pixel_face(false, true);
        assert_eq!(raised.fill, sunk.fill);
        assert_eq!(raised.light, sunk.shade);
        assert_eq!(raised.shade, sunk.light);
        let chosen = pixel_face(true, false);
        assert_ne!(chosen.fill, raised.fill);
        assert_ne!(chosen.ink, raised.ink);
        let chosen_down = pixel_face(true, true);
        assert_eq!(chosen.fill, chosen_down.fill);
        assert_eq!(chosen.light, chosen_down.shade);
    }

    #[test]
    fn an_overlay_fades_in_and_dismisses_on_the_way_out() {
        let start = Instant::now();
        let mut clock = PopClock::new();
        let opening = clock.advance(true, start);
        assert_eq!(opening.step, 1);
        assert!(!opening.dismiss);
        let shown = clock.advance(true, start + std::time::Duration::from_millis(150));
        assert_eq!(shown.step, 4);
        clock.request_close(start + std::time::Duration::from_millis(200));
        let leaving = clock.advance(true, start + std::time::Duration::from_millis(200));
        assert_eq!(leaving.step, 3);
        let done = clock.advance(true, start + std::time::Duration::from_millis(350));
        assert!(done.dismiss);
        assert_eq!(done.step, 0);
        assert_eq!(clock.advance(false, start).step, 0);
    }

    #[test]
    fn debug_steps_back_to_the_list_then_the_menu() {
        assert_eq!(debug_back(true, "escape", "f7"), DebugBack::Field);
        assert_eq!(debug_back(false, "escape", "f7"), DebugBack::Menu);
        assert_eq!(debug_back(false, "f7", "f7"), DebugBack::Menu);
        assert_eq!(debug_back(true, "f7", "f7"), DebugBack::Stay);
        assert_eq!(debug_back(false, "a", "f7"), DebugBack::Stay);
        assert_eq!(debug_back(false, "f9", "f9"), DebugBack::Menu);
    }
}
