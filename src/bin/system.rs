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
#[allow(dead_code)]
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

pub(crate) fn pause_key_while_open(open: bool) -> Option<SystemInput> {
    if open {
        Some(SystemInput::Resume)
    } else {
        None
    }
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
}
