//! The controlling side's cursor state machine.
//!
//! This machine owns the physical mouse and keyboard and makes every decision about where the
//! cursor is. While the cursor is on another machine it tracks a virtual position in the
//! unified layout and sends absolute positions, so it can always bring the cursor home without
//! any help from the other side (lock screens, hung compositors and dropped links included).
//!
//! Pure and synchronous: the capture backend feeds it input and carries out the returned
//! actions, which keeps it testable and lets the backend decide swallowing inline.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

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
    /// The cursor just crossed this (local) point on an edge: left for another computer, or
    /// arrived back from one. Only for show.
    Crossed {
        at: Point,
        arrived: bool,
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

/// How long injected input must have been still before local motion counts as the user
/// reaching for this computer's own mouse. On Linux we can't otherwise tell injected pointer
/// motion from real motion.
const TAKEOVER_QUIET: Duration = Duration::from_millis(250);

enum Exit {
    /// Where it lands, and the point on our own edge it leaves from.
    Cross(DisplayRef, Point, Point),
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
    /// Where each peer was put, by a person or automatically. Displays change size and number
    /// under a placement (resolution, scaling, a monitor plugged in), so the layout uses the
    /// nearest valid spot to it; it comes back here when the displays do.
    placements: HashMap<String, Point>,
    reachable: HashSet<String>,
    state: State,
    /// Keys and buttons held locally; their releases stay local even after a grab.
    local_held: HashSet<u16>,
    remote_held: HashSet<u16>,
    seq: u64,
    /// The computer controlling this one, if any. Local input stays local then (an injected
    /// cursor reaching an edge must not bounce off to a third machine), except that this
    /// computer's own mouse can take the cursor back to the controlling computer.
    controlled_by: Option<String>,
    /// When the controlling computer last moved our cursor.
    injected_at: Option<Instant>,
    /// When the cursor last crossed on to another computer.
    entered_at: Option<Instant>,
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
            placements: HashMap::new(),
            reachable: HashSet::new(),
            state: State::Local,
            local_held: HashSet::new(),
            remote_held: HashSet::new(),
            seq: 0,
            controlled_by: None,
            injected_at: None,
            entered_at: None,
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

    /// This machine's displays changed. Returns actions if the cursor had to come home.
    pub fn set_local_displays(&mut self, displays: Vec<DisplayInfo>) -> Vec<Action> {
        let following = self.remote_offset();
        self.layout.machines[SELF].displays = displays;
        self.settle();
        self.follow_layout(following)
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
        let following = self.remote_offset();
        self.placements.insert(id.into(), offset);
        let machine = Machine {
            id: id.into(),
            displays,
            offset,
        };
        match self.layout.machine_index(id) {
            Some(i) => self.layout.machines[i] = machine,
            None => self.layout.machines.push(machine),
        }
        self.settle();
        self.follow_layout(following)
    }

    /// Take a peer out of the layout (unpaired). Returns actions if the cursor was on it.
    pub fn remove_peer(&mut self, id: &str) -> Vec<Action> {
        let Some(i) = self.layout.machine_index(id) else {
            return vec![];
        };
        let actions = if self.active_peer() == Some(id) {
            self.go_home(None)
        } else {
            vec![]
        };
        self.layout.machines.remove(i);
        self.placements.remove(id);
        self.reachable.remove(id);
        if let State::Remote { on, .. } = &mut self.state
            && on.machine > i
        {
            on.machine -= 1;
        }
        self.settle();
        actions
    }

    /// Put every peer at its placement, or the nearest valid spot if its displays (or ours)
    /// have changed so that it would overlap something or be out of reach.
    fn settle(&mut self) {
        if self.layout.machines[SELF].displays.is_empty() {
            return;
        }
        for m in 1..self.layout.machines.len() {
            let machine = &self.layout.machines[m];
            let wanted = self
                .placements
                .get(&machine.id)
                .copied()
                .unwrap_or(machine.offset);
            self.layout.machines[m].offset = wanted;
            if self.layout.machines[m].displays.is_empty() || self.layout.well_placed(m, SELF) {
                continue;
            }
            if let Some(offset) = self.layout.snap(m, SELF, wanted) {
                self.layout.machines[m].offset = offset;
            }
        }
    }

    /// The offset of the machine the cursor is on, if it's on another one.
    fn remote_offset(&self) -> Option<Point> {
        match self.state {
            State::Remote { on, .. } => Some(self.layout.machines[on.machine].offset),
            State::Local => None,
        }
    }

    /// After the layout changed: the cursor stays where it was on the other machine's
    /// screen (moving with it if the machine moved), or comes home if its display is gone.
    fn follow_layout(&mut self, old_offset: Option<Point>) -> Vec<Action> {
        let (State::Remote { on, pos, home }, Some(old_offset)) = (self.state, old_offset) else {
            return vec![];
        };
        let machine = &self.layout.machines[on.machine];
        if on.display >= machine.displays.len() {
            return self.go_home(None);
        }
        let pos = self
            .layout
            .rect(on)
            .clamp(pos.offset(machine.offset.minus(old_offset)));
        self.state = State::Remote { on, pos, home };
        vec![]
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

    /// `id` says it's no longer ours to control (someone else took it over). If the cursor is
    /// on it, come home; it already knows, so don't tell it.
    pub fn sent_home_by(&mut self, id: &str) -> Vec<Action> {
        if self.active_peer() == Some(id) {
            self.go_home(Some(false))
        } else {
            vec![]
        }
    }

    /// Another computer has started (`Some`) or stopped (`None`) controlling this one. If the
    /// cursor was away on some computer, it comes home first: the other side has taken over.
    pub fn set_controlled_by(&mut self, by: Option<&str>) -> Vec<Action> {
        self.controlled_by = by.map(str::to_string);
        self.injected_at = None;
        let Some(by) = by else { return vec![] };
        match self.state {
            State::Remote { on, home, .. } => {
                let id = &self.layout.machines[on.machine].id;
                // The computer our cursor is on crossing into us is either its own mouse taking
                // the cursor back (only possible once we'd been still there a while; it already
                // has the cursor, so no Leave) or both of us crossing at the same moment (tell
                // it, so we both end up home).
                let tell = if id == by {
                    self.entered_at
                        .is_some_and(|t| t.elapsed() < TAKEOVER_QUIET)
                } else {
                    self.reachable.contains(id)
                };
                self.leave(on, home, tell)
            }
            State::Local => vec![],
        }
    }

    /// The point on an edge of ours leading to `peer` nearest `at` (local coordinates), if
    /// `at` is close to one: where a cursor `peer` moves in or out of us crossed.
    pub fn edge_towards(&self, peer: &str, at: Point) -> Option<Point> {
        /// Further than a fast flick's single step from the edge means it didn't cross there
        /// (sent home by the hotkey, say).
        const NEAR: f64 = 100.0;
        let to = self.layout.machine_index(peer)?;
        let p = at.offset(self.layout.machines[SELF].offset);
        let (q, d) = self
            .layout
            .edges_between(SELF, to)
            .into_iter()
            .map(|(a, b)| {
                let (ax, ay) = (b.x - a.x, b.y - a.y);
                let len = ax * ax + ay * ay;
                let t = if len > 0.0 {
                    (((p.x - a.x) * ax + (p.y - a.y) * ay) / len).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let q = Point::new(a.x + t * ax, a.y + t * ay);
                (q, (p.x - q.x).hypot(p.y - q.y))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))?;
        (d <= NEAR).then(|| self.layout.to_local(SELF, q))
    }

    /// The controlling computer just moved our cursor.
    pub fn note_injected(&mut self) {
        self.injected_at = Some(Instant::now());
    }

    pub fn handle(&mut self, input: Input) -> Outcome {
        if let Some(by) = &self.controlled_by
            && self.state == State::Local
        {
            return self.handle_controlled(by.clone(), input);
        }
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
                    Some(Exit::Cross(to, point, edge)) => {
                        let peer = self.layout.machines[to.machine].id.clone();
                        let local = self.layout.to_local(to.machine, point);
                        self.state = State::Remote {
                            on: to,
                            pos: point,
                            home: at,
                        };
                        self.entered_at = Some(Instant::now());
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
                                Action::Crossed {
                                    at: edge,
                                    arrived: false,
                                },
                            ],
                        }
                    }
                    None => Outcome::default(),
                }
            }
        }
    }

    /// Being controlled: only a push from this computer's own mouse towards the controlling
    /// computer does anything, taking the cursor over there.
    fn handle_controlled(&mut self, by: String, input: Input) -> Outcome {
        let Input::Motion { at, dx, dy, .. } = input else {
            return Outcome::default();
        };
        let quiet = self
            .injected_at
            .is_none_or(|t| t.elapsed() >= TAKEOVER_QUIET);
        let crosses = matches!(
            self.find_exit(at, dx, dy),
            Some(Exit::Cross(to, ..)) if self.layout.machines[to.machine].id == by
        );
        if !quiet || !crosses {
            return Outcome::default();
        }
        self.controlled_by = None;
        self.handle_local(input)
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
            sides.push((Side::Left, at.y, Point::new(r.x, at.y)));
        }
        if dx > 0.0 && at.x >= r.right() - 1.5 {
            sides.push((Side::Right, at.y, Point::new(r.right(), at.y)));
        }
        if dy < 0.0 && at.y <= r.y + 0.5 {
            sides.push((Side::Above, at.x, Point::new(at.x, r.y)));
        }
        if dy > 0.0 && at.y >= r.bottom() - 1.5 {
            sides.push((Side::Below, at.x, Point::new(at.x, r.bottom())));
        }
        sides.into_iter().find_map(|(side, along, edge)| {
            let c = self.layout.neighbour(from, side, along)?;
            if c.to.machine == SELF {
                return None;
            }
            let id = &self.layout.machines[c.to.machine].id;
            Some(if self.reachable.contains(id) {
                Exit::Cross(c.to, c.point, edge)
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
            Input::Motion {
                dx, dy, dragging, ..
            } => out.actions = self.move_remote(on, pos, home, dx, dy, dragging),
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
        dragging: bool,
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
        // Whichever side it's pushed furthest past that leads somewhere, so pushing out of a
        // corner still crosses. A drag stays on this computer, like one here stays here.
        let mut sides: Vec<_> = over.iter().filter(|(_, by)| *by > 0.0).collect();
        sides.sort_by(|a, b| b.1.total_cmp(&a.1));
        let crossing = sides.into_iter().find_map(|(side, _)| {
            let along = match side {
                Side::Left | Side::Right => clamped.y,
                Side::Above | Side::Below => clamped.x,
            };
            self.layout.neighbour(on, *side, along).filter(|c| {
                let to = c.to.machine;
                to == on.machine
                    || !dragging
                        && (to == SELF || self.reachable.contains(&self.layout.machines[to].id))
            })
        });
        match crossing {
            Some(c) if c.to.machine == SELF => {
                let warp = self.layout.to_local(SELF, c.point);
                let mut actions = self.leave(on, warp, true);
                actions.push(Action::Crossed {
                    at: warp,
                    arrived: true,
                });
                actions
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
                self.entered_at = Some(Instant::now());
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
                },
                Action::Crossed {
                    at: Point::new(0.0, 510.0),
                    arrived: true
                }
            ]
        );
        assert_eq!(c.active_peer(), None);
    }

    #[test]
    fn finds_the_edge_a_peer_moved_the_cursor_across() {
        let c = desk();
        // The iMac is left of the MacBook: its cursor last seen a flick in from our left edge.
        assert_eq!(
            c.edge_towards("imac", Point::new(40.0, 300.0)),
            Some(Point::new(0.0, 300.0))
        );
        // Mid-screen: it went home some other way.
        assert_eq!(c.edge_towards("imac", Point::new(700.0, 300.0)), None);
        assert_eq!(c.edge_towards("nobody", Point::new(0.0, 300.0)), None);
    }

    #[test]
    fn crossing_reports_the_point_on_the_edge_itself() {
        let mut c = desk();
        // Half a point in from the edge still crosses; the ripple starts on the edge.
        let out = c.handle(motion(0.4, 500.0, -3.0, 0.0));
        assert_eq!(
            out.actions.last(),
            Some(&Action::Crossed {
                at: Point::new(0.0, 500.0),
                arrived: false
            })
        );
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
    fn no_crossing_while_being_controlled() {
        let mut c = desk();
        c.set_peer(
            "third",
            vec![display("t", 0.0, 0.0, 800.0, 600.0, true)],
            Point::default(),
        );
        let offset = c.offset_beside("third", Side::Right, None).unwrap();
        c.set_peer(
            "third",
            vec![display("t", 0.0, 0.0, 800.0, 600.0, true)],
            offset,
        );
        c.set_reachable("third", true);
        c.set_controlled_by(Some("imac"));
        // Never on to a third computer.
        assert_eq!(
            c.handle(motion(1511.0, 500.0, 3.0, 0.0)),
            Outcome::default()
        );
        // Nor back to the controlling one while it's still moving our cursor.
        c.note_injected();
        assert_eq!(c.handle(motion(0.0, 500.0, -3.0, 0.0)), Outcome::default());
        c.set_controlled_by(None);
        enter(&mut c);
    }

    #[test]
    fn own_mouse_takes_the_cursor_back_to_the_controlling_computer() {
        let mut c = desk();
        c.set_controlled_by(Some("imac"));
        c.injected_at = Instant::now().checked_sub(TAKEOVER_QUIET);
        enter(&mut c);
        assert_eq!(c.active_peer(), Some("imac"));
        // Now controlling it, not controlled by it: pushing back brings the cursor home.
        let out = c.handle(motion(0.0, 500.0, 150.0, 0.0));
        assert!(
            out.actions
                .iter()
                .any(|a| matches!(a, Action::Release { .. }))
        );
    }

    #[test]
    fn being_taken_over_brings_the_cursor_home() {
        let mut c = desk();
        enter(&mut c);
        // Both crossed at once: tell it, so it comes home too.
        let actions = c.set_controlled_by(Some("imac"));
        assert!(actions.contains(&Action::Send {
            peer: "imac".into(),
            msg: Message::Leave
        }));
        c.set_controlled_by(None);

        enter(&mut c);
        c.entered_at = Instant::now().checked_sub(TAKEOVER_QUIET);
        // The iMac's own mouse took the cursor back to us: no Leave for it, just come home.
        assert_eq!(
            c.set_controlled_by(Some("imac")),
            vec![Action::Release {
                warp: Point::new(0.0, 500.0)
            }]
        );
        assert_eq!(c.active_peer(), None);
        assert_eq!(c.handle(motion(10.0, 500.0, 3.0, 0.0)), Outcome::default());
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

    fn imac(c: &Controller) -> Machine {
        let m = c.layout.machine_index("imac").unwrap();
        c.layout.machines[m].clone()
    }

    #[test]
    fn peer_growing_into_an_overlap_is_nudged_then_restored() {
        let mut c = desk();
        let placed = imac(&c).offset;
        let bigger = vec![display("eDP-1", 0.0, 0.0, 2560.0, 1440.0, true)];
        c.set_peer("imac", bigger, placed);
        let m = c.layout.machine_index("imac").unwrap();
        assert_ne!(imac(&c).offset, placed);
        assert!(c.layout.well_placed(m, SELF));

        // Back to how it was: back where it was put.
        let imac_displays = vec![display("eDP-1", 0.0, 0.0, 1920.0, 1080.0, true)];
        c.set_peer("imac", imac_displays, placed);
        assert_eq!(imac(&c).offset, placed);
    }

    #[test]
    fn local_display_appearing_under_a_peer_nudges_it() {
        let mut c = desk();
        let placed = imac(&c).offset;
        let actions = c.set_local_displays(vec![
            display("builtin", 0.0, 0.0, 1512.0, 982.0, true),
            display("side", -1000.0, 0.0, 1000.0, 982.0, false),
        ]);
        assert!(actions.is_empty());
        let m = c.layout.machine_index("imac").unwrap();
        assert_ne!(imac(&c).offset, placed);
        assert!(c.layout.well_placed(m, SELF));
    }

    #[test]
    fn cursor_on_a_peer_that_grows_can_still_come_home() {
        let mut c = desk();
        enter(&mut c);
        let placed = imac(&c).offset;
        let bigger = vec![display("eDP-1", 0.0, 0.0, 2560.0, 1440.0, true)];
        assert!(c.set_peer("imac", bigger, placed).is_empty());
        assert_eq!(c.active_peer(), Some("imac"));
        // Push right across the whole (now wider) screen: home, not stuck.
        for _ in 0..10 {
            c.handle(motion(0.0, 500.0, 100.0, 0.0));
        }
        assert_eq!(c.active_peer(), None);
    }

    #[test]
    fn cursor_moves_with_a_peer_that_is_moved() {
        let mut c = desk();
        enter(&mut c);
        let moved = imac(&c).offset.offset(Point::new(0.0, 40.0));
        let displays = imac(&c).displays;
        assert!(c.set_peer("imac", displays, moved).is_empty());
        assert_eq!(imac(&c).offset, moved);
        // Same spot on the iMac's own screen as before the move.
        let out = c.handle(motion(0.0, 500.0, -100.0, 10.0));
        assert!(
            matches!(out.actions[0], Action::Motion { motion: Motion { x, y, .. }, .. } if x == 1819.0 && y == 510.0),
            "{:?}",
            out.actions
        );
    }

    #[test]
    fn peer_losing_the_display_the_cursor_is_on_sends_it_home() {
        let mut c = desk();
        enter(&mut c);
        let placed = imac(&c).offset;
        let actions = c.set_peer("imac", vec![], placed);
        assert!(matches!(actions.last(), Some(Action::Release { .. })));
        assert_eq!(c.active_peer(), None);
    }

    #[test]
    fn removing_a_peer_takes_it_out_of_the_layout() {
        let mut c = desk();
        enter(&mut c);
        let actions = c.remove_peer("imac");
        assert!(matches!(actions.last(), Some(Action::Release { .. })));
        assert_eq!(c.active_peer(), None);
        assert!(c.layout.machine_index("imac").is_none());
        // Its old edge no longer leads anywhere.
        let out = c.handle(motion(0.0, 500.0, -3.0, 0.0));
        assert_eq!(out, Outcome::default());
    }

    #[test]
    fn removing_another_peer_keeps_the_cursor_where_it_is() {
        let mut c = Controller::new(
            "mac",
            vec![display("builtin", 0.0, 0.0, 1512.0, 982.0, true)],
        );
        let screen = vec![display("s", 0.0, 0.0, 1000.0, 800.0, true)];
        c.set_peer("left", screen.clone(), Point::new(-1000.0, 0.0));
        c.set_peer("right", screen, Point::new(1512.0, 0.0));
        c.set_reachable("right", true);
        c.handle(motion(1511.0, 400.0, 3.0, 0.0));
        assert_eq!(c.active_peer(), Some("right"));
        assert!(c.remove_peer("left").is_empty());
        assert_eq!(c.active_peer(), Some("right"));
        // Still tracking it on the right-hand machine: pushing left comes home.
        c.handle(motion(1511.0, 400.0, -10.0, 0.0));
        assert_eq!(c.active_peer(), None);
    }

    fn drag(dx: f64, dy: f64) -> Input {
        Input::Motion {
            at: Point::new(0.0, 500.0),
            dx,
            dy,
            dragging: true,
        }
    }

    #[test]
    fn dragging_on_the_remote_stays_there() {
        let mut c = desk();
        enter(&mut c);
        c.handle(motion(0.0, 500.0, -100.0, 0.0));
        let out = c.handle(drag(150.0, 0.0));
        assert!(
            matches!(out.actions.as_slice(), [Action::Motion { motion: Motion { x, .. }, .. }] if *x == 1919.0),
            "{:?}",
            out.actions
        );
        assert_eq!(c.active_peer(), Some("imac"));
        // Let go, and the same push comes home.
        c.handle(motion(0.0, 500.0, 50.0, 0.0));
        assert_eq!(c.active_peer(), None);
    }

    #[test]
    fn pushing_out_of_a_corner_crosses_where_it_can() {
        let mut c = Controller::new(
            "mac",
            vec![display("builtin", 0.0, 0.0, 1512.0, 982.0, true)],
        );
        let imac = vec![display("eDP-1", 0.0, 0.0, 1920.0, 1080.0, true)];
        c.set_peer("imac", imac, Point::new(-1920.0, 0.0));
        c.set_reachable("imac", true);
        c.handle(motion(0.0, 10.0, -3.0, 0.0));
        assert_eq!(c.active_peer(), Some("imac"));
        // Up and to the right from its top-right corner: further up than right, but only the
        // right-hand side leads anywhere.
        c.handle(motion(0.0, 10.0, 5.0, -40.0));
        assert_eq!(c.active_peer(), None);
    }

    #[test]
    fn sent_home_by_the_peer_the_cursor_is_on() {
        let mut c = desk();
        assert!(c.sent_home_by("imac").is_empty());
        enter(&mut c);
        assert!(c.sent_home_by("other").is_empty());
        assert_eq!(c.active_peer(), Some("imac"));
        let actions = c.sent_home_by("imac");
        // Home without a Leave: the iMac already knows.
        assert!(matches!(actions.as_slice(), [Action::Release { .. }]));
        assert_eq!(c.active_peer(), None);
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
