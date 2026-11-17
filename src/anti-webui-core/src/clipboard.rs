//! Binary clipboard offers. Text editing continues to use the text clipboard API.
//!
//! Reads choose the first available format in the caller's preference order. The returned
//! offer names let callers distinguish file-copy/cut and image representations. An empty
//! preference list only lists formats. Tasks and callbacks belong to their reactive owner.
use crate::{
    event::ClipboardKind,
    platform::PlatformRequest,
    reactive::on_cleanup,
    task::{Completer, Task},
    tree::with_doc,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardError {
    Unsupported,
    NotFocused,
    Unavailable,
    TooLarge,
    Io(String),
}
impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ClipboardError {}

/// One representation of the same selection; bytes are never decoded as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardData {
    pub mime: String,
    pub bytes: Arc<[u8]>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardOffer {
    pub formats: Vec<String>,
    pub data: Option<ClipboardData>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum ClipboardRequest {
    Write {
        id: u64,
        kind: ClipboardKind,
        data: Vec<ClipboardData>,
    },
    Read {
        id: u64,
        kind: ClipboardKind,
        formats: Vec<String>,
        max_bytes: usize,
    },
}
impl ClipboardRequest {
    pub fn failed(self, error: ClipboardError) -> ClipboardReply {
        match self {
            Self::Write { id, .. } => ClipboardReply::Write {
                id,
                result: Err(error),
            },
            Self::Read { id, .. } => ClipboardReply::Read {
                id,
                result: Err(error),
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum ClipboardReply {
    Write {
        id: u64,
        result: Result<(), ClipboardError>,
    },
    Read {
        id: u64,
        result: Result<ClipboardOffer, ClipboardError>,
    },
}
type Reads = HashMap<u64, Completer<Result<ClipboardOffer, ClipboardError>>>;
type Writes = HashMap<u64, Completer<Result<(), ClipboardError>>>;
thread_local! {
    static NEXT: Cell<u64> = const { Cell::new(0) };
    static READS: RefCell<Reads> = RefCell::default();
    static WRITES: RefCell<Writes> = RefCell::default();
}
/// Offers all representations together. Requires keyboard focus and an input serial on Wayland.
/// Empty or duplicate MIME names are rejected; an empty offer clears the selection.
pub fn write_clipboard(
    kind: ClipboardKind,
    data: Vec<ClipboardData>,
) -> Task<Result<(), ClipboardError>> {
    let mut names = std::collections::HashSet::new();
    if data
        .iter()
        .any(|d| d.mime.is_empty() || d.mime.contains('\0') || !names.insert(&d.mime))
    {
        return Task::ready(Err(ClipboardError::Io(
            "empty or duplicate clipboard MIME name".into(),
        )));
    }
    let id = NEXT.with(|n| n.replace(n.get() + 1));
    let (task, done) = Task::pending();
    WRITES.with(|w| w.borrow_mut().insert(id, done));
    on_cleanup(move || {
        WRITES.with(|w| w.borrow_mut().remove(&id));
    });
    with_doc(|d| {
        d.request(PlatformRequest::Clipboard(ClipboardRequest::Write {
            id,
            kind,
            data,
        }))
    });
    task
}
/// Reads at most `max_bytes`, choosing formats in preference order. No match returns `data: None`.
/// With no preferences this only probes the offer. Unsupported backends return an explicit error.
pub fn read_clipboard(
    kind: ClipboardKind,
    formats: Vec<String>,
    max_bytes: usize,
) -> Task<Result<ClipboardOffer, ClipboardError>> {
    let id = NEXT.with(|n| n.replace(n.get() + 1));
    let (task, done) = Task::pending();
    READS.with(|r| r.borrow_mut().insert(id, done));
    on_cleanup(move || {
        READS.with(|r| r.borrow_mut().remove(&id));
    });
    with_doc(|d| {
        d.request(PlatformRequest::Clipboard(ClipboardRequest::Read {
            id,
            kind,
            formats,
            max_bytes,
        }))
    });
    task
}
#[doc(hidden)]
pub fn complete(reply: ClipboardReply) {
    match reply {
        ClipboardReply::Write { id, result } => {
            let done = WRITES.with(|w| w.borrow_mut().remove(&id));
            if let Some(done) = done {
                done.complete(result);
            }
        }
        ClipboardReply::Read { id, result } => {
            let done = READS.with(|r| r.borrow_mut().remove(&id));
            if let Some(done) = done {
                done.complete(result);
            }
        }
    }
}

/// Selects a local offer with the same semantics as reading an external selection.
#[doc(hidden)]
pub fn select(
    data: &[ClipboardData],
    preferences: &[String],
    max_bytes: usize,
) -> Result<ClipboardOffer, ClipboardError> {
    let selected = preferences
        .iter()
        .find_map(|m| data.iter().find(|d| &d.mime == m));
    if selected.is_some_and(|d| d.bytes.len() > max_bytes) {
        return Err(ClipboardError::TooLarge);
    }
    Ok(ClipboardOffer {
        formats: data.iter().map(|d| d.mime.clone()).collect(),
        data: selected.cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefers_requested_order_and_keeps_binary_bytes() {
        let data = vec![
            ClipboardData {
                mime: "text/uri-list".into(),
                bytes: b"file:///a".as_slice().into(),
            },
            ClipboardData {
                mime: "image/png".into(),
                bytes: [0, 255, 13, 10].as_slice().into(),
            },
        ];
        let offer = select(&data, &["image/png".into(), "text/uri-list".into()], 4).unwrap();
        assert_eq!(offer.data, Some(data[1].clone()));
        assert_eq!(
            select(&data, &["image/png".into()], 3),
            Err(ClipboardError::TooLarge)
        );
        assert!(select(&data, &[], 0).unwrap().data.is_none());
        assert!(
            select(&data, &["missing".into()], 0)
                .unwrap()
                .data
                .is_none()
        );
    }
    #[test]
    fn completion_is_delivered_to_its_request_and_dropped_with_owner() {
        use crate::reactive::create_root;
        let (task, owner) = create_root(|| read_clipboard(ClipboardKind::Clipboard, vec![], 0));
        let id = NEXT.with(|n| n.get() - 1);
        complete(ClipboardReply::Read {
            id: id + 1,
            result: Ok(ClipboardOffer::default()),
        });
        crate::channel::pump();
        assert!(!task.is_done());
        complete(ClipboardReply::Read {
            id,
            result: Err(ClipboardError::NotFocused),
        });
        crate::channel::pump();
        assert_eq!(task.get(), Some(Err(ClipboardError::NotFocused)));
        owner.dispose();
        assert!(READS.with(|r| r.borrow().is_empty()));
    }
}
