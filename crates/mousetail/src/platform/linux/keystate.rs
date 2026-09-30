//! The virtual keyboard's modifier state, worked out from the keymap it uses.
//!
//! A virtual keyboard has to tell the compositor its modifiers itself. Doing that from the
//! keymap (rather than assuming Right Alt is Alt, say) gets AltGr right on layouts that use
//! it for `@`, `€` and friends, and Caps Lock and Num Lock right on every layout.

use xkbcommon::xkb;

/// Linux Num Lock (`input-event-codes.h`).
const NUMLOCK: u16 = 69;

pub struct KeyState {
    keymap: xkb::Keymap,
    state: xkb::State,
}

/// Modifiers as the virtual keyboard sends them: depressed, latched, locked, layout group.
pub type Serialized = (u32, u32, u32, u32);

impl KeyState {
    pub fn new(keymap: xkb::Keymap) -> Self {
        let state = Self::fresh(&keymap);
        Self { keymap, state }
    }

    /// Nothing held, Caps Lock off, Num Lock on: a Mac's keypad types digits (it has no Num
    /// Lock), and it's how PC keyboards usually start.
    fn fresh(keymap: &xkb::Keymap) -> xkb::State {
        let mut state = xkb::State::new(keymap);
        let numlock = xkb::Keycode::new(u32::from(NUMLOCK) + 8);
        state.update_key(numlock, xkb::KeyDirection::Down);
        state.update_key(numlock, xkb::KeyDirection::Up);
        state
    }

    pub fn reset(&mut self) {
        self.state = Self::fresh(&self.keymap);
    }

    /// Follow a key press or release. True if the modifiers changed.
    pub fn key(&mut self, code: u16, down: bool) -> bool {
        let direction = if down {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        let changed = self
            .state
            .update_key(xkb::Keycode::new(u32::from(code) + 8), direction);
        changed & (xkb::STATE_MODS_EFFECTIVE | xkb::STATE_LAYOUT_EFFECTIVE) != 0
    }

    pub fn serialize(&self) -> Serialized {
        (
            self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        )
    }

    #[cfg(test)]
    fn sym(&self, code: u16) -> xkb::Keysym {
        self.state
            .key_get_one_sym(xkb::Keycode::new(u32::from(code) + 8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mousetail_core::keys::ev;

    fn layout(name: &str) -> KeyState {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            name,
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("keymap");
        KeyState::new(keymap)
    }

    const KEY_Q: u16 = 16;
    const KEY_A: u16 = 30;
    const KEY_KP1: u16 = 79;
    const RIGHTALT: u16 = 100;

    #[test]
    fn altgr_types_level_three_on_german_layouts() {
        let mut k = layout("de");
        assert!(k.key(RIGHTALT, true));
        assert_eq!(k.sym(KEY_Q), xkb::Keysym::at);
        assert!(k.key(RIGHTALT, false));
        assert_eq!(k.sym(KEY_Q), xkb::Keysym::q);
    }

    #[test]
    fn keypad_types_digits_from_the_start() {
        let k = layout("us");
        assert_eq!(k.sym(KEY_KP1), xkb::Keysym::KP_1);
    }

    #[test]
    fn caps_lock_toggles_and_reset_clears_it() {
        let mut k = layout("us");
        k.key(ev::CAPSLOCK, true);
        k.key(ev::CAPSLOCK, false);
        assert_eq!(k.sym(KEY_A), xkb::Keysym::A);
        k.reset();
        assert_eq!(k.sym(KEY_A), xkb::Keysym::a);
        assert_eq!(k.sym(KEY_KP1), xkb::Keysym::KP_1);
    }
}
