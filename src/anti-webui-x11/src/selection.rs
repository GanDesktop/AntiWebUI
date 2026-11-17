//! The clipboard and the primary selection (see "クリップボード" in
//! `docs/design/35-platform-x11.md`). Everything happens in the event loop, with INCR for what
//! doesn't fit in one request.

use crate::state::State;
use anti_webui_core::clipboard::{
    ClipboardData, ClipboardError, ClipboardOffer, ClipboardReply, ClipboardRequest,
};
use anti_webui_core::event::{ClipboardKind, WindowEvent};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use x11rb::connection::RequestConnection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, PropMode, Property,
    SelectionNotifyEvent, SelectionRequestEvent, Window,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

/// How long a paste waits for the owner.
const PASTE_WAIT: Duration = Duration::from_secs(5);

/// What we hand out, and since when.
struct Owned {
    data: Vec<u8>,
    formats: Vec<(Atom, ClipboardData)>,
    time: u32,
}

/// What a conversion is for.
#[derive(Clone, PartialEq)]
enum Purpose {
    Paste(ClipboardKind),
    /// What was dropped, in this format.
    Drop(String),
    Formats {
        id: u64,
        kind: ClipboardKind,
        preferences: Vec<String>,
        max_bytes: usize,
    },
    Binary {
        id: u64,
        kind: ClipboardKind,
        formats: Vec<String>,
        mime: String,
        max_bytes: usize,
    },
}

/// A paste or a drop on its way.
struct Paste {
    purpose: Purpose,
    /// What was asked for: `UTF8_STRING` first, then `STRING`.
    target: Atom,
    /// What came so far of an INCR transfer.
    incr: Option<Vec<u8>>,
    /// Latin-1 rather than UTF-8.
    latin1: bool,
    until: Instant,
}

/// An INCR transfer to another program.
struct Sending {
    requestor: Window,
    property: Atom,
    kind: Atom,
    data: Vec<u8>,
    at: usize,
}

#[derive(Default)]
pub(crate) struct Selections {
    owned: HashMap<Atom, Owned>,
    pastes: HashMap<Atom, Paste>,
    sending: Vec<Sending>,
    /// What `SAVE_TARGETS` was asked of the clipboard manager, when closing.
    saving: Option<Instant>,
}

/// UTF-8 to Latin-1, with `?` for what it doesn't have.
fn latin1(text: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(text)
        .chars()
        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
        .collect()
}

fn from_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

impl State {
    fn selection_atom(&self, kind: ClipboardKind) -> Atom {
        match kind {
            ClipboardKind::Clipboard => self.atoms.CLIPBOARD,
            ClipboardKind::Primary => AtomEnum::PRIMARY.into(),
        }
    }

    fn property_of(&self, purpose: &Purpose) -> Atom {
        match purpose {
            Purpose::Paste(ClipboardKind::Clipboard) => self.atoms.ANTIWEBUI_SELECTION,
            Purpose::Paste(ClipboardKind::Primary) => self.atoms.ANTIWEBUI_PRIMARY,
            Purpose::Drop(_) => self.atoms.ANTIWEBUI_DROP,
            Purpose::Formats { kind, .. } | Purpose::Binary { kind, .. } => match kind {
                ClipboardKind::Clipboard => self.atoms.ANTIWEBUI_SELECTION,
                ClipboardKind::Primary => self.atoms.ANTIWEBUI_PRIMARY,
            },
        }
    }

    /// The largest property we put in one request.
    fn chunk(&self) -> usize {
        (self.conn.maximum_request_bytes() / 4).clamp(4096, 256 * 1024)
    }

    pub(crate) fn set_clipboard(&mut self, kind: ClipboardKind, text: String) {
        let selection = self.selection_atom(kind);
        let time = self.last_time;
        let _ = self.conn.set_selection_owner(self.window, selection, time);
        self.selections.owned.insert(
            selection,
            Owned {
                data: text.into_bytes(),
                formats: Vec::new(),
                time,
            },
        );
    }

    pub(crate) fn request_paste(&mut self, kind: ClipboardKind) {
        let selection = self.selection_atom(kind);
        if let Some(owned) = self.selections.owned.get(&selection) {
            let data = if owned.formats.is_empty() {
                Some(owned.data.as_slice())
            } else {
                owned
                    .formats
                    .iter()
                    .find(|(a, _)| {
                        *a == self.atoms.UTF8_STRING
                            || *a == self.atoms.TEXT_PLAIN_UTF8
                            || *a == self.atoms.TEXT_PLAIN
                    })
                    .map(|(_, d)| d.bytes.as_ref())
            };
            let Some(data) = data else {
                return;
            };
            let text = String::from_utf8_lossy(data).into_owned();
            self.events.push(WindowEvent::Paste { kind, text });
            return;
        }
        let target = self.atoms.UTF8_STRING;
        let time = self.last_time;
        self.ask(selection, Purpose::Paste(kind), target, false, time);
    }

    pub(crate) fn clipboard(&mut self, request: ClipboardRequest) {
        match request {
            ClipboardRequest::Write { id, kind, data } => {
                let result = (|| {
                    let mut formats = Vec::new();
                    for d in data {
                        let atom = self
                            .conn
                            .intern_atom(false, d.mime.as_bytes())
                            .map_err(io)?
                            .reply()
                            .map_err(io)?
                            .atom;
                        formats.push((atom, d));
                    }
                    let selection = self.selection_atom(kind);
                    let owner = if formats.is_empty() {
                        NONE
                    } else {
                        self.window
                    };
                    self.conn
                        .set_selection_owner(owner, selection, self.last_time)
                        .map_err(io)?
                        .check()
                        .map_err(io)?;
                    if self
                        .conn
                        .get_selection_owner(selection)
                        .map_err(io)?
                        .reply()
                        .map_err(io)?
                        .owner
                        != owner
                    {
                        return Err(ClipboardError::Unavailable);
                    }
                    if formats.is_empty() {
                        self.selections.owned.remove(&selection);
                    } else {
                        self.selections.owned.insert(
                            selection,
                            Owned {
                                data: Vec::new(),
                                formats,
                                time: self.last_time,
                            },
                        );
                    }
                    Ok(())
                })();
                self.events
                    .push(WindowEvent::Clipboard(ClipboardReply::Write { id, result }));
            }
            ClipboardRequest::Read {
                id,
                kind,
                formats,
                max_bytes,
            } => {
                let selection = self.selection_atom(kind);
                if self.selections.pastes.contains_key(&selection) {
                    self.binary_done(id, Err(ClipboardError::Busy));
                    return;
                }
                self.ask(
                    selection,
                    Purpose::Formats {
                        id,
                        kind,
                        preferences: formats,
                        max_bytes,
                    },
                    self.atoms.TARGETS,
                    false,
                    self.last_time,
                );
            }
        }
    }

    fn binary_done(&mut self, id: u64, result: Result<ClipboardOffer, ClipboardError>) {
        self.events
            .push(WindowEvent::Clipboard(ClipboardReply::Read { id, result }));
    }

    fn failed(&mut self, purpose: Purpose, error: ClipboardError) {
        match purpose {
            Purpose::Formats { id, .. } | Purpose::Binary { id, .. } => {
                self.binary_done(id, Err(error))
            }
            other => self.deliver(other, Vec::new(), false),
        }
    }

    /// Reads what was dropped, as `target`.
    pub(crate) fn ask_for_drop(&mut self, mime: String, target: Atom, time: u32) {
        let selection = self.atoms.XdndSelection;
        self.ask(selection, Purpose::Drop(mime), target, false, time);
    }

    fn ask(&mut self, selection: Atom, purpose: Purpose, target: Atom, latin1: bool, time: u32) {
        let property = self.property_of(&purpose);
        let _ = self
            .conn
            .convert_selection(self.window, selection, target, property, time);
        if let Some(old) = self.selections.pastes.remove(&selection) {
            self.failed(old.purpose, ClipboardError::Busy);
        }
        self.selections.pastes.insert(
            selection,
            Paste {
                purpose,
                target,
                incr: None,
                latin1,
                until: Instant::now() + PASTE_WAIT,
            },
        );
    }

    /// The next time a paste gives up.
    pub(crate) fn selection_deadline(&self) -> Option<Instant> {
        self.selections.pastes.values().map(|p| p.until).min()
    }

    /// Pastes nobody answered come out empty.
    pub(crate) fn give_up_pastes(&mut self, now: Instant) {
        let late: Vec<Atom> = self
            .selections
            .pastes
            .iter()
            .filter(|(_, p)| p.until <= now)
            .map(|(&a, _)| a)
            .collect();
        for selection in late {
            if let Some(p) = self.selections.pastes.remove(&selection) {
                self.failed(p.purpose, ClipboardError::Unavailable);
            }
        }
    }

    /// The owner put what we asked for in our property, or couldn't.
    pub(crate) fn selection_notify(&mut self, e: &SelectionNotifyEvent) {
        if e.requestor != self.window {
            return;
        }
        if e.selection == self.atoms.CLIPBOARD_MANAGER {
            self.selections.saving = None;
            return;
        }
        let Some(paste) = self.selections.pastes.get(&e.selection) else {
            return;
        };
        if e.target != paste.target
            || (e.property != NONE && e.property != self.property_of(&paste.purpose))
        {
            return;
        }
        let limit = read_limit(&paste.purpose);
        let purpose = paste.purpose.clone();
        if e.property == NONE {
            let retry =
                paste.target == self.atoms.UTF8_STRING && matches!(purpose, Purpose::Paste(_));
            if retry {
                self.ask(e.selection, purpose, AtomEnum::STRING.into(), true, e.time);
            } else {
                self.selections.pastes.remove(&e.selection);
                self.failed(purpose, ClipboardError::Unavailable);
            }
            return;
        }
        let Some(reply) = self
            .conn
            .get_property(
                true,
                self.window,
                e.property,
                AtomEnum::ANY,
                0,
                property_length(limit),
            )
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return;
        };
        if reply.bytes_after > 0 || (reply.type_ != self.atoms.INCR && reply.value.len() > limit) {
            self.selections.pastes.remove(&e.selection);
            self.failed(purpose, ClipboardError::TooLarge);
            return;
        }
        if reply.type_ == self.atoms.INCR {
            // The rest comes as new values of the property, which deleting it asks for.
            if let Some(p) = self.selections.pastes.get_mut(&e.selection) {
                p.incr = Some(Vec::new());
                p.until = Instant::now() + PASTE_WAIT;
            }
            return;
        }
        self.finish_paste(e.selection, reply.value);
    }

    fn finish_paste(&mut self, selection: Atom, bytes: Vec<u8>) {
        let Some(p) = self.selections.pastes.remove(&selection) else {
            return;
        };
        self.deliver(p.purpose, bytes, p.latin1);
    }

    fn deliver(&mut self, purpose: Purpose, bytes: Vec<u8>, latin1: bool) {
        match purpose {
            Purpose::Paste(kind) => {
                let text = if latin1 {
                    from_latin1(&bytes)
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                self.events.push(WindowEvent::Paste { kind, text });
            }
            Purpose::Formats {
                id,
                kind,
                preferences,
                max_bytes,
            } => {
                let atoms: Vec<u32> = bytes
                    .chunks_exact(4)
                    .map(|b| u32::from_ne_bytes(b.try_into().expect("four bytes")))
                    .collect();
                let mut formats = Vec::new();
                for a in atoms {
                    if a == self.atoms.TARGETS || a == self.atoms.TIMESTAMP {
                        continue;
                    }
                    if let Some(name) = self
                        .conn
                        .get_atom_name(a)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .and_then(|r| String::from_utf8(r.name).ok())
                    {
                        formats.push(name);
                    }
                }
                let Some(mime) = preferences.iter().find(|m| formats.contains(m)).cloned() else {
                    self.binary_done(
                        id,
                        Ok(ClipboardOffer {
                            formats,
                            data: None,
                        }),
                    );
                    return;
                };
                let atom = self
                    .conn
                    .intern_atom(false, mime.as_bytes())
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| r.atom);
                let Some(atom) = atom else {
                    self.binary_done(id, Err(ClipboardError::Unavailable));
                    return;
                };
                self.ask(
                    self.selection_atom(kind),
                    Purpose::Binary {
                        id,
                        kind,
                        formats,
                        mime,
                        max_bytes,
                    },
                    atom,
                    false,
                    self.last_time,
                );
            }
            Purpose::Binary {
                id,
                formats,
                mime,
                max_bytes,
                ..
            } => {
                let result = if bytes.len() > max_bytes {
                    Err(ClipboardError::TooLarge)
                } else {
                    Ok(ClipboardOffer {
                        formats,
                        data: Some(ClipboardData {
                            mime,
                            bytes: bytes.into(),
                        }),
                    })
                };
                self.binary_done(id, result);
            }
            Purpose::Drop(mime) => {
                self.events.push(WindowEvent::DropData { mime, bytes });
                self.drop_read();
            }
        }
    }

    /// A property changed: a piece of an INCR transfer either way.
    pub(crate) fn selection_property(&mut self, window: Window, atom: Atom, deleted: bool) {
        if window == self.window && !deleted {
            let incoming = self
                .selections
                .pastes
                .iter()
                .find(|(_, p)| p.incr.is_some() && self.property_of(&p.purpose) == atom)
                .map(|(&s, _)| s);
            let Some(selection) = incoming else { return };
            let p = &self.selections.pastes[&selection];
            let limit = read_limit(&p.purpose).saturating_sub(p.incr.as_ref().map_or(0, Vec::len));
            let Some(reply) = self
                .conn
                .get_property(
                    true,
                    self.window,
                    atom,
                    AtomEnum::ANY,
                    0,
                    property_length(limit),
                )
                .ok()
                .and_then(|c| c.reply().ok())
            else {
                return;
            };
            if reply.bytes_after > 0 || reply.value.len() > limit {
                if let Some(p) = self.selections.pastes.remove(&selection) {
                    self.failed(p.purpose, ClipboardError::TooLarge);
                }
                return;
            }
            if reply.value.is_empty() {
                let bytes = self
                    .selections
                    .pastes
                    .get_mut(&selection)
                    .and_then(|p| p.incr.take())
                    .unwrap_or_default();
                self.finish_paste(selection, bytes);
            } else if let Some(p) = self.selections.pastes.get_mut(&selection) {
                p.incr.get_or_insert_with(Vec::new).extend(reply.value);
                p.until = Instant::now() + PASTE_WAIT;
            }
            return;
        }
        if !deleted {
            return;
        }
        let chunk = self.chunk();
        let Some(i) = self
            .selections
            .sending
            .iter()
            .position(|s| s.requestor == window && s.property == atom)
        else {
            return;
        };
        let s = &mut self.selections.sending[i];
        let end = (s.at + chunk).min(s.data.len());
        let piece = &s.data[s.at..end];
        let _ =
            self.conn
                .change_property8(PropMode::REPLACE, s.requestor, s.property, s.kind, piece);
        let done = piece.is_empty();
        s.at = end;
        if done {
            self.selections.sending.remove(i);
        }
    }

    /// Another program asks for what we own.
    pub(crate) fn selection_request(&mut self, e: &SelectionRequestEvent) {
        let a = &self.atoms;
        let property = if e.property == NONE {
            e.target
        } else {
            e.property
        };
        if e.selection == a.XdndSelection {
            let answered = if e.target == a.TARGETS {
                let targets = self.drag_targets();
                self.conn
                    .change_property32(
                        PropMode::REPLACE,
                        e.requestor,
                        property,
                        AtomEnum::ATOM,
                        &targets,
                    )
                    .is_ok()
            } else {
                match self.drag_data(e.target) {
                    Some(data) => self.send(e.requestor, property, e.target, data.to_vec()),
                    None => false,
                }
            };
            return self.notify(e, answered.then_some(property));
        }
        let a = &self.atoms;
        let owned = self.selections.owned.get(&e.selection);
        let answered = match owned {
            Some(owned) if e.time == CURRENT_TIME || e.time >= owned.time => {
                if e.target == a.TARGETS {
                    let mut targets = vec![
                        a.TARGETS,
                        a.TIMESTAMP,
                        a.UTF8_STRING,
                        a.TEXT_PLAIN_UTF8,
                        a.TEXT_PLAIN,
                        u32::from(AtomEnum::STRING),
                        a.TEXT,
                    ];
                    if !owned.formats.is_empty() {
                        targets = vec![a.TARGETS, a.TIMESTAMP];
                        targets.extend(owned.formats.iter().map(|(a, _)| *a));
                    }
                    self.conn
                        .change_property32(
                            PropMode::REPLACE,
                            e.requestor,
                            property,
                            AtomEnum::ATOM,
                            &targets,
                        )
                        .is_ok()
                } else if e.target == a.TIMESTAMP {
                    self.conn
                        .change_property32(
                            PropMode::REPLACE,
                            e.requestor,
                            property,
                            AtomEnum::INTEGER,
                            &[owned.time],
                        )
                        .is_ok()
                } else if !owned.formats.is_empty() {
                    let data = owned
                        .formats
                        .iter()
                        .find(|(a, _)| *a == e.target)
                        .map(|(_, d)| d.bytes.to_vec());
                    match data {
                        Some(data) => self.send(e.requestor, property, e.target, data),
                        None => false,
                    }
                } else if [a.UTF8_STRING, a.TEXT_PLAIN_UTF8, a.TEXT_PLAIN, a.TEXT]
                    .contains(&e.target)
                {
                    let kind = if e.target == a.TEXT {
                        a.UTF8_STRING
                    } else {
                        e.target
                    };
                    let data = owned.data.clone();
                    self.send(e.requestor, property, kind, data)
                } else if e.target == u32::from(AtomEnum::STRING) {
                    let data = latin1(&owned.data);
                    self.send(e.requestor, property, AtomEnum::STRING.into(), data)
                } else {
                    false
                }
            }
            _ => false,
        };
        self.notify(e, answered.then_some(property));
    }

    fn notify(&self, e: &SelectionRequestEvent, property: Option<Atom>) {
        let notify = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: e.time,
            requestor: e.requestor,
            selection: e.selection,
            target: e.target,
            property: property.unwrap_or(NONE),
        };
        let _ = self
            .conn
            .send_event(false, e.requestor, EventMask::NO_EVENT, notify);
    }

    /// Puts `data` in the requestor's property, in pieces if it is large.
    fn send(&mut self, requestor: Window, property: Atom, kind: Atom, data: Vec<u8>) -> bool {
        if data.len() <= self.chunk() {
            return self
                .conn
                .change_property8(PropMode::REPLACE, requestor, property, kind, &data)
                .is_ok();
        }
        let aux = ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
        let _ = self.conn.change_window_attributes(requestor, &aux);
        let len = data.len() as u32;
        let sent = self
            .conn
            .change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                self.atoms.INCR,
                &[len],
            )
            .is_ok();
        if sent {
            self.selections.sending.push(Sending {
                requestor,
                property,
                kind,
                data,
                at: 0,
            });
        }
        sent
    }

    /// Someone else owns the selection now.
    pub(crate) fn selection_clear(&mut self, selection: Atom) {
        self.selections.owned.remove(&selection);
    }

    /// Hands the clipboard to the clipboard manager, if there is one, before closing. Whether to
    /// wait for it.
    pub(crate) fn save_clipboard(&mut self) -> bool {
        let a = &self.atoms;
        if !self.selections.owned.contains_key(&a.CLIPBOARD) {
            return false;
        }
        let manager = self
            .conn
            .get_selection_owner(a.CLIPBOARD_MANAGER)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|r| r.owner != NONE);
        if !manager {
            return false;
        }
        let _ = self.conn.convert_selection(
            self.window,
            a.CLIPBOARD_MANAGER,
            a.SAVE_TARGETS,
            NONE,
            self.last_time,
        );
        self.selections.saving = Some(Instant::now() + Duration::from_secs(1));
        true
    }

    /// Whether the clipboard manager is still taking the clipboard.
    pub(crate) fn saving_clipboard(&self) -> bool {
        self.selections
            .saving
            .is_some_and(|until| Instant::now() < until)
            || !self.selections.sending.is_empty()
    }
}

fn io(e: impl std::fmt::Display) -> ClipboardError {
    ClipboardError::Io(e.to_string())
}
fn read_limit(p: &Purpose) -> usize {
    match p {
        Purpose::Binary { max_bytes, .. } => *max_bytes,
        Purpose::Formats { .. } => 256 * 1024,
        _ => 64 * 1024 * 1024,
    }
}
fn property_length(limit: usize) -> u32 {
    limit
        .saturating_add(1)
        .div_ceil(4)
        .min(u32::MAX as usize / 4) as u32
}

/// Whether a `PropertyNotify` says the property went.
pub(crate) fn deleted(state: Property) -> bool {
    state == Property::DELETE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_goes_both_ways_with_what_it_has() {
        assert_eq!(latin1("café ✓".as_bytes()), b"caf\xe9 ?");
        assert_eq!(from_latin1(b"caf\xe9"), "café");
    }
}
