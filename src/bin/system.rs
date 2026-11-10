use gpui::{div, prelude::*, px, rgb, Context, Window};
use super::{
    cheat_panel, link_panel, picture_option_label, AppView, CheatView, Command, LinkSeat,
    SetLockedSpeed, ToggleColorCorrect, ToggleLcdGhost, ToggleRumbleShake, ACCENT, BG, MUTED,
    SPEEDS, TEXT,
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

pub(crate) fn system_link_highlight(field: SystemField) -> Option<bool> {
    match field {
        SystemField::LinkName => Some(true),
        SystemField::LinkAddress => Some(false),
        SystemField::None | SystemField::CheatName | SystemField::CheatCode => None,
    }
}

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

pub(crate) fn pause_key_while_open(open: bool) -> Option<SystemInput> {
    if open {
        Some(SystemInput::Resume)
    } else {
        None
    }
}

pub(crate) fn system_page(
    cx: &mut Context<AppView>,
    title: &str,
    locked_speed: u8,
    lcd_ghost: bool,
    color_correct: bool,
    rumble_shake: bool,
    address: &str,
    name: &str,
    field: SystemField,
    line: &str,
    card_line: &str,
    link_error: Option<&str>,
    lan_ip: &str,
    hosts: &[(String, String)],
    in_room: bool,
    seats: &[LinkSeat],
    cheat_name: &str,
    cheat_code: &str,
    gameshark: bool,
    action_replay: bool,
    cheats: &[CheatView],
    cheat_error: Option<&str>,
) -> impl IntoElement {
    div()
        .id("system-page")
        .when(field != SystemField::None, |page| {
            page.key_context("SystemTyping")
        })
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(16.))
        .p(px(24.))
        .bg(rgb(BG))
        .text_color(rgb(TEXT))
        .child(div().text_lg().child("系统"))
        .when(!title.is_empty(), |page| {
            page.child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(title.to_string()),
            )
        })
        .child(
            div()
                .id("system-resume")
                .px(px(18.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(rgb(ACCENT))
                .text_color(rgb(BG))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.system_field = SystemField::None;
                    let _ = this.commands.send(Command::SystemKey);
                    cx.notify();
                }))
                .child("恢复"),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child("速度"))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .children(SPEEDS.map(|speed| speed_button(cx, speed, locked_speed))),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child("画面"))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .child(picture_button(
                    cx,
                    "system-ghost",
                    &picture_option_label("液晶余辉", lcd_ghost),
                    |window, cx| window.dispatch_action(Box::new(ToggleLcdGhost), cx),
                ))
                .child(picture_button(
                    cx,
                    "system-color",
                    &picture_option_label("色彩校正", color_correct),
                    |window, cx| window.dispatch_action(Box::new(ToggleColorCorrect), cx),
                ))
                .child(picture_button(
                    cx,
                    "system-rumble",
                    &picture_option_label("画面震动", rumble_shake),
                    |window, cx| window.dispatch_action(Box::new(ToggleRumbleShake), cx),
                )),
        )
        .child(link_panel(
            cx,
            address,
            name,
            system_link_highlight(field),
            line,
            card_line,
            link_error,
            lan_ip,
            hosts,
            in_room,
            seats,
        ))
        .child(cheat_panel(
            cx,
            cheat_name,
            cheat_code,
            gameshark,
            action_replay,
            system_cheat_highlight(field),
            cheats,
            cheat_error,
        ))
}

fn speed_button(cx: &mut Context<AppView>, speed: u8, selected: u8) -> impl IntoElement {
    div()
        .id(("system-speed", speed as usize))
        .px(px(14.))
        .py(px(8.))
        .rounded(px(6.))
        .bg(rgb(0x202A2C))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| {
            this.system_field = SystemField::None;
            window.dispatch_action(Box::new(SetLockedSpeed(speed)), cx);
        }))
        .child(picture_option_label(&speed.to_string(), speed == selected))
}

fn picture_button(
    cx: &mut Context<AppView>,
    id: &'static str,
    label: &str,
    dispatch: fn(&mut Window, &mut Context<AppView>),
) -> impl IntoElement {
    let label = label.to_string();
    div()
        .id(id)
        .px(px(14.))
        .py(px(8.))
        .rounded(px(6.))
        .bg(rgb(0x202A2C))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| {
            this.system_field = SystemField::None;
            dispatch(window, cx);
        }))
        .child(label)
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
}
