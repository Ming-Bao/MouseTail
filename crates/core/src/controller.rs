//! The controlling side's cursor state machine.
//!
//! This machine owns the physical mouse and keyboard and makes every decision about where the
//! cursor is. While the cursor is on another machine it tracks a virtual position in the
//! unified layout and sends absolute positions, so it can always bring the cursor home without
//! any help from the other side (lock screens, hung compositors and dropped links included).
//!
//! Pure and synchronous: the capture backend feeds it input and carries out the returned
//! actions, which keeps it testable and lets the backend decide swallowing inline.

use std::collections::HashSet;

use crate::keys::ev;
use crate::layout::{DisplayRef, Layout, Machine, Point, Side};
use crate::proto::{DisplayInfo, Message, Motion, Scroll};

/// Local input as seen by the capture backend.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// `at` is the local cursor position; `dx`/`dy` the (accelerated) movement; `dragging`
    /// whether a mouse button is actually held (as reported by the OS with the event).
    Motion {
        at: Point,
        dx: f64,
        dy: f64,
        dragging: bool,
    },
    Button {
        code: u16,
        down: bool,
    },
    Scroll(Scroll),
    Key {
        code: u16,
        down: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Freeze and hide the local cursor; input is now swallowed.
    Grab,
    /// Give the cursor back locally at this (local) point.
    Release {
        warp: Point,
    },
    Send {
        peer: String,
        msg: Message,
    },
    Motion {
        peer: String,
        motion: Motion,
    },
    /// The user pushed towards a paired computer that isn't connected: try waking it.
    Wake {
        peer: String,
    },
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Drop the event instead of delivering it locally.
    pub swallow: bool,
    pub actions: Vec<Action>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Local,
    Remote {
        on: DisplayRef,
        pos: Point,
        /// Where to put the local cursor if we have to come home abruptly.
        home: Point,
    },
}

const SELF: usize = 0;

enum Exit {
    Cross(DisplayRef, Point),
    Offline(String),
}

fn sane(d: &DisplayInfo) -> bool {
    let r = d.rect;
    [r.x, r.y, r.w, r.h]
        .iter()
        .all(|v| v.is_finite() && v.abs() < 1e6)
        && r.w >= 1.0
        && r.h >= 1.0
}

pub struct Controller {
    layout: Layout,
    reachable: HashSet<String>,
    state: State,
    /// Keys and buttons held locally; their releases stay local even after a grab.
    local_held: HashSet<u16>,
    remote_held: HashSet<u16>,
    seq: u64,
}

impl Controller {
    pub fn new(self_id: &str, displays: Vec<DisplayInfo>) -> Self {
        Self {
            layout: Layout {
                machines: vec![Machine {
                    id: self_id.into(),
                    displays,
                    offset: Point::default(),
                }],
            },
            reachable: HashSet::new(),
            state: State::Local,
            local_held: HashSet::new(),
            remote_held: HashSet::new(),
            seq: 0,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Which peer the cursor is currently on, if any.
    pub fn active_peer(&self) -> Option<&str> {
        match self.state {
            State::Remote { on, .. } => Some(&self.layout.machines[on.machine].id),
            State::Local => None,
        }
    }

    pub fn set_local_displays(&mut self, displays: Vec<DisplayInfo>) {
        self.layout.machines[SELF].displays = displays;
    }

    /// Add or update a peer's displays and placement. Returns actions if the cursor had to
    /// come home because its display vanished.
    pub fn set_peer(&mut self, id: &str, displays: Vec<DisplayInfo>, offset: Point) -> Vec<Action> {
        // Peers are trusted but not infallible: ignore displays that would break the maths.
        let displays: Vec<DisplayInfo> = displays.into_iter().filter(sane).collect();
        let offset = if offset.x.is_finite() && offset.y.is_finite() {
            offset
        } else {
            Point::default()
        };
        let machine = Machine {
            id: id.into(),
            displays,
            offset,
        };
        match self.layout.machine_index(id) {
            Some(i) => self.layout.machines[i] = machine,
            None => self.layout.machines.push(machine),
        }
        match self.state {
            State::Remote { on, .. }
                if self.layout.machines[on.machine].id == id
                    && on.display >= self.layout.machines[on.machine].displays.len() =>
            {
                self.go_home(None)
            }
            _ => vec![],
        }
    }

    /// Offset placing `id` beside one of this machine's displays (default: the primary),
    /// centred on it and then nudged if needed so it overlaps nothing.
    pub fn offset_beside(&self, id: &str, side: Side, display: Option<usize>) -> Option<Point> {
        let m = self.layout.machine_index(id)?;
        let d = display.or(self.layout.machines[SELF].primary())?;
        let anchor = DisplayRef {
            machine: SELF,
            display: d,
        };
        if d >= self.layout.machines[SELF].displays.len() {
            return None;
        }
        let centred = self.layout.offset_beside(anchor, m, side)?;
        self.layout.snap(m, SELF, centred)
    }

    /// Nearest valid placement for `id` to where it was dropped (see `Layout::snap`).
    pub fn snap(&self, id: &str, desired: Point) -> Option<Point> {
        let m = self.layout.machine_index(id)?;
        self.layout.snap(m, SELF, desired)
    }

    pub fn set_reachable(&mut self, id: &str, reachable: bool) -> Vec<Action> {
        if reachable {
            self.reachable.insert(id.into());
            return vec![];
        }
        self.reachable.remove(id);
        if self.active_peer() == Some(id) {
            // The link is gone, so there's no one to tell; just come home.
            return self.go_home(Some(false));
        }
        vec![]
    }

    /// Bring the cursor home now (hotkey, shutdown, pause).
    pub fn release(&mut self) -> Vec<Action> {
        self.go_home(None)
    }

    pub fn handle(&mut self, input: Input) -> Outcome {
        match self.state {
            State::Local => self.handle_local(input),
            State::Remote { .. } => self.handle_remote(input),
        }
    }

    fn handle_local(&mut self, input: Input) -> Outcome {
        match input {
            Input::Key { code, down } | Input::Button { code, down } => {
                if down {
                    self.local_held.insert(code);
                } else {
                    self.local_held.remove(&code);
                }
                Outcome::default()
            }
            Input::Scroll(_) => Outcome::default(),
            Input::Motion {
                at,
                dx,
                dy,
                dragging,
            } => {
                // Don't carry a drag across machines. Trust the OS's view of the buttons, so a
                // missed button-up can't block crossing for good.
                if dragging {
                    return Outcome::default();
                }
                self.local_held.retain(|c| !ev::is_button(*c));
                match self.find_exit(at, dx, dy) {
                    Some(Exit::Offline(peer)) => Outcome {
                        swallow: false,
                        actions: vec![Action::Wake { peer }],
                    },
                    Some(Exit::Cross(to, point)) => {
                        let peer = self.layout.machines[to.machine].id.clone();
                        let local = self.layout.to_local(to.machine, point);
                        self.state = State::Remote {
                            on: to,
                            pos: point,
                            home: at,
                        };
                        Outcome {
                            swallow: true,
                            actions: vec![
                                Action::Grab,
                                Action::Send {
                                    peer,
                                    msg: Message::Enter {
                                        x: local.x,
                                        y: local.y,
                                    },
                                },
                            ],
                        }
                    }
                    None => Outcome::default(),
                }
            }
        }
    }

    /// If the local cursor is pushing against an edge that leads to another computer, where
    /// does it land (or which offline computer is it reaching for)?
    fn find_exit(&self, at: Point, dx: f64, dy: f64) -> Option<Exit> {
        let from = self
            .layout
            .locate_on(SELF, at)
            .or_else(|| self.layout.nearest_on(SELF, at))?;
        let r = self.layout.rect(from);
        let mut sides = Vec::with_capacity(2);
        if dx < 0.0 && at.x <= r.x + 0.5 {
            sides.push((Side::Left, at.y));
        }
        if dx > 0.0 && at.x >= r.right() - 1.5 {
            sides.push((Side::Right, at.y));
        }
        if dy < 0.0 && at.y <= r.y + 0.5 {
            sides.push((Side::Above, at.x));
        }
        if dy > 0.0 && at.y >= r.bottom() - 1.5 {
            sides.push((Side::Below, at.x));
        }
        sides.into_iter().find_map(|(side, along)| {
            let c = self.layout.neighbour(from, side, along)?;
            if c.to.machine == SELF {
                return None;
            }
            let id = &self.layout.machines[c.to.machine].id;
            Some(if self.reachable.contains(id) {
                Exit::Cross(c.to, c.point)
            } else {
                Exit::Offline(id.clone())
            })
        })
    }

    fn handle_remote(&mut self, input: Input) -> Outcome {
        let State::Remote { on, pos, home } = self.state else {
            unreachable!()
        };
        let peer = self.layout.machines[on.machine].id.clone();
        let local = self.layout.to_local(on.machine, pos);
        let mut out = Outcome {
            swallow: true,
            actions: vec![],
        };
        match input {
            Input::Motion { dx, dy, .. } => out.actions = self.move_remote(on, pos, home, dx, dy),
            Input::Key { code, down } | Input::Button { code, down } => {
                if !down && self.local_held.remove(&code) {
                    // Pressed before we crossed: let the release reach this machine.
                    out.swallow = false;
                    return out;
                }
                if down {
                    // A fresh press means any earlier local hold of this key is stale (its
                    // release was missed); from now on it belongs to the remote.
                    self.local_held.remove(&code);
                }
                if down && code == ev::ESC && self.hotkey_held() {
                    out.actions = self.go_home(None);
                    return out;
                }
                if down {
                    self.remote_held.insert(code);
                } else {
                    self.remote_held.remove(&code);
                }
                let msg = if ev::is_button(code) {
                    Message::Button {
                        code,
                        down,
                        x: local.x,
                        y: local.y,
                    }
                } else {
                    Message::Key { code, down }
                };
                out.actions.push(Action::Send { peer, msg });
            }
            Input::Scroll(s) => out.actions.push(Action::Send {
                peer,
                msg: Message::Scroll(s),
            }),
        }
        out
    }

    /// Ctrl+Option+Escape brings the cursor home from anywhere.
    fn hotkey_held(&self) -> bool {
        self.remote_held.iter().any(|c| ev::is_ctrl(*c))
            && self.remote_held.iter().any(|c| ev::is_alt(*c))
    }

    fn move_remote(
        &mut self,
        on: DisplayRef,
        pos: Point,
        home: Point,
        dx: f64,
        dy: f64,
    ) -> Vec<Action> {
        let target = Point::new(pos.x + dx, pos.y + dy);
        if let Some(d) = self.layout.locate_on(on.machine, target) {
            return self.moved_to(d, target, home);
        }
        let r = self.layout.rect(on);
        let over = [
            (Side::Left, r.x - target.x),
            (Side::Right, target.x - (r.right() - 1.0)),
            (Side::Above, r.y - target.y),
            (Side::Below, target.y - (r.bottom() - 1.0)),
        ];
        let clamped = r.clamp(target);
        let crossing = over
            .iter()
            .filter(|(_, by)| *by > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .and_then(|(side, _)| {
                let along = match side {
                    Side::Left | Side::Right => clamped.y,
                    Side::Above | Side::Below => clamped.x,
                };
                self.layout.neighbour(on, *side, along)
            });
        match crossing {
            Some(c) if c.to.machine == SELF => {
                let warp = self.layout.to_local(SELF, c.point);
                self.leave(on, warp, true)
            }
            Some(c) if c.to.machine == on.machine => self.moved_to(c.to, c.point, home),
            Some(c)
                if self
                    .reachable
                    .contains(&self.layout.machines[c.to.machine].id) =>
            {
                // Straight on to another remote machine.
                let from = self.layout.machines[on.machine].id.clone();
                let to = self.layout.machines[c.to.machine].id.clone();
                let local = self.layout.to_local(c.to.machine, c.point);
                self.remote_held.clear();
                self.state = State::Remote {
                    on: c.to,
                    pos: c.point,
                    home,
                };
                vec![
                    Action::Send {
                        peer: from,
                        msg: Message::Leave,
                    },
                    Action::Send {
                        peer: to,
                        msg: Message::Enter {
                            x: local.x,
                            y: local.y,
                        },
                    },
                ]
            }
            _ => self.moved_to(on, clamped, home),
        }
    }

    fn moved_to(&mut self, on: DisplayRef, pos: Point, home: Point) -> Vec<Action> {
        let State::Remote { pos: prev, .. } = self.state else {
            unreachable!()
        };
        self.state = State::Remote { on, pos, home };
        if prev == pos {
            return vec![];
        }
        self.seq += 1;
        let local = self.layout.to_local(on.machine, pos);
        vec![Action::Motion {
            peer: self.layout.machines[on.machine].id.clone(),
            motion: Motion {
                seq: self.seq,
                x: local.x,
                y: local.y,
            },
        }]
    }

    fn leave(&mut self, on: DisplayRef, warp: Point, tell_peer: bool) -> Vec<Action> {
        let peer = self.layout.machines[on.machine].id.clone();
        self.state = State::Local;
        self.remote_held.clear();
        let mut actions = Vec::with_capacity(2);
        if tell_peer {
            actions.push(Action::Send {
                peer,
                msg: Message::Leave,
            });
        }
        actions.push(Action::Release { warp });
        actions
    }

    /// Return to the point we left from. `tell_peer: None` means "if it's reachable".
    fn go_home(&mut self, tell_peer: Option<bool>) -> Vec<Action> {
        match self.state {
            State::Local => vec![],
            State::Remote { on, home, .. } => {
                let tell = tell_peer.unwrap_or_else(|| {
                    self.reachable
                        .contains(&self.layout.machines[on.machine].id)
                });
                self.leave(on, home, tell)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Rect;

    fn display(id: &str, x: f64, y: f64, w: f64, h: f64, primary: bool) -> DisplayInfo {
        DisplayInfo {
            id: id.into(),
            name: id.into(),
            rect: Rect::new(x, y, w, h),
            scale: 1.0,
            primary,
        }
    }

    fn desk() -> Controller {
        let mut c = Controller::new(
            "mac",
            vec![
                display("builtin", 0.0, 0.0, 1512.0, 982.0, true),
                display("dell", -587.0, -1440.0, 2560.0, 1440.0, false),
            ],
        );
        let imac = vec![display("eDP-1", 0.0, 0.0, 1920.0, 1080.0, true)];
        c.set_peer("imac", imac.clone(), Point::default());
        let offset = c.offset_beside("imac", Side::Left, None).unwrap();
        c.set_peer("imac", imac, offset);
        c.set_reachable("imac", true);
        c
    }

    fn motion(x: f64, y: f64, dx: f64, dy: f64) -> Input {
        Input::Motion {
            at: Point::new(x, y),
            dx,
            dy,
            dragging: false,
        }
    }

    fn enter(c: &mut Controller) {
        let out = c.handle(motion(0.0, 500.0, -3.0, 0.0));
        assert!(out.swallow);
        assert_eq!(out.actions[0], Action::Grab);
        assert_eq!(
            out.actions[1],
            Action::Send {
                peer: "imac".into(),
                msg: Message::Enter {
                    x: 1919.0,
                    y: 500.0
                }
            }
        );
    }

    #[test]
    fn crosses_into_imac_at_left_edge() {
        let mut c = desk();
        assert_eq!(c.handle(motion(0.0, 500.0, 0.0, 3.0)), Outcome::default());
        assert_eq!(c.handle(motion(10.0, 500.0, -3.0, 0.0)), Outcome::default());
        enter(&mut c);
        assert_eq!(c.active_peer(), Some("imac"));
    }

    #[test]
    fn pushing_towards_an_offline_peer_asks_to_wake_it() {
        let mut c = desk();
        c.set_reachable("imac", false);
        let out = c.handle(motion(0.0, 500.0, -3.0, 0.0));
        assert!(!out.swallow);
        assert_eq!(
            out.actions,
            vec![Action::Wake {
                peer: "imac".into()
            }]
        );
        assert_eq!(c.active_peer(), None);
    }

    #[test]
    fn moves_then_returns_home_at_right_edge() {
        let mut c = desk();
        enter(&mut c);
        let out = c.handle(motion(0.0, 500.0, -100.0, 10.0));
        assert!(
            matches!(out.actions[0], Action::Motion { motion: Motion { x, y, .. }, .. } if x == 1819.0 && y == 510.0)
        );
        let out = c.handle(motion(0.0, 500.0, 150.0, 0.0));
        assert_eq!(
            out.actions,
            vec![
                Action::Send {
                    peer: "imac".into(),
                    msg: Message::Leave
                },
                Action::Release {
                    warp: Point::new(0.0, 510.0)
                }
            ]
        );
        assert_eq!(c.active_peer(), None);
    }

    #[test]
    fn slides_along_edges_without_a_neighbour() {
        let mut c = desk();
        enter(&mut c);
        // Far left: iMac's left edge leads nowhere, so clamp.
        let out = c.handle(motion(0.0, 500.0, -5000.0, 0.0));
        assert!(
            matches!(out.actions[0], Action::Motion { motion: Motion { x, .. }, .. } if x == 0.0)
        );
        // Right edge below the MacBook (iMac is taller): no way back there.
        c.handle(motion(0.0, 500.0, 1919.0, 540.0));
        let out = c.handle(motion(0.0, 500.0, 50.0, 0.0));
        // Already against the edge, so nothing moves and nothing is sent.
        assert!(out.swallow && out.actions.is_empty());
        assert_eq!(c.active_peer(), Some("imac"));
    }

    #[test]
    fn keys_go_to_peer_but_prior_holds_release_locally() {
        let mut c = desk();
        // Shift held locally while crossing.
        c.handle(Input::Key {
            code: ev::LEFTSHIFT,
            down: true,
        });
        enter(&mut c);
        let out = c.handle(Input::Key {
            code: 30,
            down: true,
        });
        assert!(out.swallow);
        assert_eq!(
            out.actions,
            vec![Action::Send {
                peer: "imac".into(),
                msg: Message::Key {
                    code: 30,
                    down: true
                }
            }]
        );
        let out = c.handle(Input::Key {
            code: ev::LEFTSHIFT,
            down: false,
        });
        assert!(!out.swallow);
        assert!(out.actions.is_empty());
    }

    #[test]
    fn hotkey_and_disconnect_bring_cursor_home() {
        let mut c = desk();
        enter(&mut c);
        c.handle(Input::Key {
            code: ev::LEFTCTRL,
            down: true,
        });
        c.handle(Input::Key {
            code: ev::LEFTALT,
            down: true,
        });
        let out = c.handle(Input::Key {
            code: ev::ESC,
            down: true,
        });
        assert!(out.actions.contains(&Action::Release {
            warp: Point::new(0.0, 500.0)
        }));
        assert_eq!(c.active_peer(), None);

        enter(&mut c);
        let actions = c.set_reachable("imac", false);
        assert_eq!(
            actions,
            vec![Action::Release {
                warp: Point::new(0.0, 500.0)
            }]
        );
    }

    #[test]
    fn no_crossing_while_dragging() {
        let mut c = desk();
        let drag = Input::Motion {
            at: Point::new(0.0, 500.0),
            dx: -3.0,
            dy: 0.0,
            dragging: true,
        };
        assert_eq!(c.handle(drag), Outcome::default());
        // A missed button-up doesn't block crossing once the OS says nothing is held.
        c.handle(Input::Button {
            code: ev::BTN_LEFT,
            down: true,
        });
        enter(&mut c);
    }

    #[test]
    fn stale_local_hold_does_not_eat_remote_release() {
        let mut c = desk();
        // Local press whose release the tap never saw.
        c.handle(Input::Key {
            code: 30,
            down: true,
        });
        enter(&mut c);
        c.handle(Input::Key {
            code: 30,
            down: true,
        });
        let out = c.handle(Input::Key {
            code: 30,
            down: false,
        });
        assert!(out.swallow);
        assert_eq!(
            out.actions,
            vec![Action::Send {
                peer: "imac".into(),
                msg: Message::Key {
                    code: 30,
                    down: false
                }
            }]
        );
    }

    #[test]
    fn broken_peer_displays_are_ignored() {
        let mut c = desk();
        let bad = vec![
            display("zero", 0.0, 0.0, 0.0, 0.0, true),
            display("nan", f64::NAN, 0.0, 100.0, 100.0, false),
        ];
        c.set_peer("imac", bad, Point::new(f64::NAN, 0.0));
        assert!(c.layout().machines[1].displays.is_empty());
        assert_eq!(c.snap("imac", Point::new(f64::NAN, 1.0)), None);
    }
}
