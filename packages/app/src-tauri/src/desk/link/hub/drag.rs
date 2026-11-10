//! Files dragged across (the transfer itself is `files.rs`).
//!
//! **Out** (the pointer leaves holding files): a strip along the edge facing a linked computer is shown while a
//! button is held near it (`RemoteHost::drag_strip`), so the drag in progress can be read (`dragged_files`). Pushed
//! through the edge with files, the pointer crosses: `DragStart` names them, the drag here is cancelled
//! (`cancel_local_drag`; the files stay where they are), and the transfer starts at once. Released over the other
//! computer, the drop is its business; come home first, and `DragCancel` says the drag is off.
//!
//! **In** (`DropIn`): the files are staged as they arrive. Once they are all here and the button is still held, a
//! real drag of them starts under the pointer; released, it drops where the pointer is. Released before the files
//! are here, dropped where nothing takes them, or on a computer that cannot start a drag, they are kept in
//! `Downloads/My Computer` and shown. Cancelled (Esc, the pointer went home first, `DragCancel`), they are deleted.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use desk_core::Side;
use desk_proto::{FileAnnounce, StreamMsg};

use super::{short, Hub, Msg};
use crate::desk::link::arrange::ALL_SIDES;
use crate::desk::link::crossing;
use crate::desk::link::files;
use crate::desk::link::wire::{write_kind, KIND_TRANSFER};
use crate::p2p::endpoint::NodeId;

/// A button held this close to an edge facing a linked computer shows the strip (local pixels).
const STRIP_NEAR: i32 = 64;
/// How wide the strip is.
const STRIP_WIDTH: i32 = 6;
/// After the strip appeared, a push through the edge waits this long for the drag to be seen before it gives up and
/// stays here (a button held for something else, like selecting text).
pub(super) const DRAG_WAIT_MS: u64 = 400;

/// A drag of files that left this computer.
pub(super) struct DragOut {
    pub token: u64,
    /// The button went up over the other computer: the drop is made, the transfer must finish.
    pub released: bool,
    pub cancel: Arc<AtomicBool>,
}

/// A drag of files arriving on this computer.
pub(super) struct DropIn {
    pub peer: NodeId,
    pub token: u64,
    pub items: Option<Vec<PathBuf>>,
    pub released: bool,
    pub cancelled: bool,
    pub dragging: bool,
    pub tried_drag: bool,
    pub finished: Option<bool>,
}

impl Hub {
    // ---- out ---------------------------------------------------------------------------------------------

    /// Called on every local look while nothing blocks crossing: keeps the strip where a held button is near an edge.
    pub(super) fn track_drag(&mut self, held: bool, local: (i32, i32)) {
        if !held {
            if self.drag_held {
                self.inner.host.drag_reset();
                self.strip = None;
            }
            self.drag_held = false;
            return;
        }
        self.drag_held = true;
        let rect = self.strip_rect(local);
        if rect != self.strip {
            if rect.is_some() && self.strip.is_none() {
                self.strip_since = self.ms();
            }
            self.inner.host.drag_strip(rect);
            self.strip = rect;
        }
    }

    /// The strip along the edge of the screen under `local` that is near it and faces a linked computer.
    fn strip_rect(&self, local: (i32, i32)) -> Option<(i32, i32, i32, i32)> {
        let screen = self.shapes.iter().find(|s| s.rect.contains(local.0, local.1))?.rect;
        let (left, top) = (screen.x, screen.y);
        let (right, bottom) = (screen.x + screen.width as i32 - 1, screen.y + screen.height as i32 - 1);
        for side in ALL_SIDES {
            let (near, edge, push) = match side {
                Side::Left => (local.0 - left, (left, local.1), (-1, 0)),
                Side::Right => (right - local.0, (right, local.1), (1, 0)),
                Side::Top => (local.1 - top, (local.0, top), (0, -1)),
                Side::Bottom => (bottom - local.1, (local.0, bottom), (0, 1)),
            };
            if near > STRIP_NEAR || !crossing::pushed_through(&self.shapes, edge, push, side) {
                continue;
            }
            if !self.live.keys().any(|p| self.inner.depart_to(p, &self.shapes, edge, &[side]).is_some()) {
                continue;
            }
            let (w, h) = (screen.width as i32, screen.height as i32);
            let r = match side {
                Side::Left => (left, top, STRIP_WIDTH, h),
                Side::Right => (right + 1 - STRIP_WIDTH, top, STRIP_WIDTH, h),
                Side::Top => (left, top, w, STRIP_WIDTH),
                Side::Bottom => (left, bottom + 1 - STRIP_WIDTH, w, STRIP_WIDTH),
            };
            return Some((r.0 + self.origin.0, r.1 + self.origin.1, r.2, r.3));
        }
        None
    }

    /// The pointer is pushed through an edge with a button held. `Ok(Some(files))`: cross with them. `Ok(None)`:
    /// wait (the drag may not have been seen yet). `Err(())`: a button held for something else, do not cross.
    pub(super) fn drag_for_crossing(&mut self) -> Result<Option<Vec<PathBuf>>, ()> {
        if let Some(f) = self.inner.host.dragged_files().filter(|f| !f.is_empty()) {
            return Ok(Some(f));
        }
        if self.strip.is_some() && self.ms().saturating_sub(self.strip_since) < DRAG_WAIT_MS {
            return Ok(None);
        }
        Err(())
    }

    /// Right after the pointer crossed to `peer` holding `items`.
    pub(super) fn start_drag_out(&mut self, peer: NodeId, items: Vec<PathBuf>) -> Option<DragOut> {
        let live = self.live.get(&peer)?;
        let token = self.inner.next_token();
        let files: Vec<FileAnnounce> = items
            .iter()
            .take(files::MAX_ANNOUNCED)
            .map(|p| FileAnnounce {
                rel_path: p.file_name().map(|n| n.to_string_lossy().chars().take(200).collect()).unwrap_or_default(),
                bytes: 0,
                is_dir: p.is_dir(),
                mtime_ms: 0,
            })
            .collect();
        let _ = live.ctl.send(StreamMsg::DragStart { token, files });
        // The drag here is over; the files stay where they are.
        self.inner.host.cancel_local_drag();
        self.inner.host.drag_reset();
        self.strip = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let link = live.link.clone();
        let flag = cancel.clone();
        let n = items.len();
        tracing::info!(peer = short(&peer).as_str(), token, items = n, "desk: dragging files to another computer");
        self.inner.spawn(async move {
            let built = tokio::task::spawn_blocking(move || files::build_manifest(token, &items)).await;
            let (manifest, sources) = match built {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => return tracing::warn!(token, error = e.as_str(), "desk: could not read the dragged files"),
                Err(_) => return,
            };
            let mut stream = match link.open_bi().await {
                Ok(s) => s,
                Err(_) => return tracing::warn!(token, "desk: could not open a stream for the dragged files"),
            };
            if write_kind(&mut stream.send, KIND_TRANSFER).await.is_err() {
                return;
            }
            match files::send(&mut stream.send, &manifest, &sources, &flag).await {
                Ok(()) => {
                    use tokio::io::AsyncWriteExt;
                    let _ = stream.send.shutdown().await;
                    // Keep the stream open until the other side has read everything and let go of it.
                    let mut sink = [0u8; 1];
                    use tokio::io::AsyncReadExt;
                    let _ = stream.recv.read(&mut sink).await;
                    tracing::info!(token, bytes = manifest.total(), "desk: dragged files sent");
                }
                Err(e) => tracing::warn!(token, error = e.as_str(), "desk: sending the dragged files stopped"),
            }
        });
        Some(DragOut { token, released: false, cancel })
    }

    /// The pointer came home: a drag that was not dropped over there is off.
    pub(super) fn end_drag_out(&mut self, peer: &NodeId, drag: Option<DragOut>) {
        let Some(d) = drag else { return };
        if d.released {
            return;
        }
        d.cancel.store(true, Ordering::SeqCst);
        if let Some(l) = self.live.get(peer) {
            let _ = l.ctl.send(StreamMsg::DragCancel { token: d.token });
        }
    }

    // ---- in ----------------------------------------------------------------------------------------------

    pub(super) fn on_drag_start(&mut self, peer: NodeId, token: u64, count: usize) {
        if !self.controlled.as_ref().is_some_and(|c| c.peer == peer) {
            return;
        }
        if let Some(mut old) = self.drop_in.take() {
            old.cancelled = true;
            self.finish_drop_in(old);
        }
        tracing::info!(peer = short(&peer).as_str(), token, items = count, "desk: files are being dragged to this computer");
        self.inner.expect_drop(peer, token);
        self.drop_in = Some(DropIn { peer, token, items: None, released: false, cancelled: false, dragging: false, tried_drag: false, finished: None });
    }

    pub(super) fn on_drop_received(&mut self, peer: NodeId, token: u64, result: Result<Vec<PathBuf>, String>) {
        let ours = self.drop_in.as_ref().is_some_and(|d| d.peer == peer && d.token == token);
        match result {
            Ok(items) if ours => {
                if let Some(d) = self.drop_in.as_mut() {
                    d.items = Some(items);
                }
                self.advance_drop_in();
            }
            Ok(items) => discard(items),
            Err(e) => {
                if ours {
                    self.drop_in = None;
                    let name = self.peer_name(&peer);
                    self.fail(&format!("The files from {name} did not come across: {e}"));
                }
            }
        }
    }

    /// The button the drag is held with went up (or the person pressed Esc: `cancel`).
    pub(super) fn drop_in_button_up(&mut self, cancel: bool) {
        let Some(d) = self.drop_in.as_mut() else { return };
        if d.released || d.cancelled {
            return;
        }
        if cancel {
            d.cancelled = true;
        } else {
            d.released = true;
        }
        if d.dragging {
            self.inner.host.end_file_drag(!cancel);
        }
        self.advance_drop_in();
    }

    pub(super) fn on_drag_cancel(&mut self, peer: NodeId, token: u64) {
        if self.drop_in.as_ref().is_some_and(|d| d.peer == peer && d.token == token) {
            self.drop_in_button_up(true);
        }
    }

    /// Being controlled ended: a drag not dropped yet is off; one already dropped finishes.
    pub(super) fn drop_in_control_ended(&mut self) {
        if self.drop_in.as_ref().is_some_and(|d| !d.released) {
            self.drop_in_button_up(true);
        }
    }

    pub(super) fn on_drop_finished(&mut self, token: u64, accepted: bool) {
        let Some(d) = self.drop_in.as_mut().filter(|d| d.token == token) else { return };
        d.dragging = false;
        d.finished = Some(accepted);
        tracing::info!(token, accepted, "desk: the drag of files ended here");
        self.advance_drop_in();
    }

    fn advance_drop_in(&mut self) {
        let Some(d) = self.drop_in.as_mut() else { return };
        if d.dragging {
            return;
        }
        let ready = d.items.is_some();
        let done = d.cancelled || d.finished.is_some() || (ready && d.released);
        if done {
            if d.cancelled && !d.released && d.finished.is_none() && !ready {
                // Cancelled before the files were here: whatever arrives later is deleted (nothing expects it).
                self.inner.clear_drop();
                self.drop_in = None;
                return;
            }
            if let Some(d) = self.drop_in.take() {
                self.finish_drop_in(d);
            }
            return;
        }
        if ready && !d.tried_drag {
            d.tried_drag = true;
            let items = d.items.clone().unwrap_or_default();
            let token = d.token;
            let tx = self.tx.clone();
            let done: crate::desk::link::remote::DragDone = Box::new(move |accepted| {
                let _ = tx.send(Msg::DropFinished { token, accepted });
            });
            match self.inner.host.begin_file_drag(items, done) {
                Ok(()) => {
                    if let Some(d) = self.drop_in.as_mut() {
                        d.dragging = true;
                    }
                }
                // Kept when the button goes up.
                Err(e) => tracing::info!(reason = e.as_str(), "desk: no drag here, the files are kept when the button goes up"),
            }
        }
    }

    /// The end of an arriving drag: accepted by a drop target (nothing to do), cancelled (deleted), or dropped where
    /// nothing took it (kept in Downloads and shown).
    fn finish_drop_in(&mut self, d: DropIn) {
        self.inner.clear_drop();
        if d.dragging {
            self.inner.host.end_file_drag(false);
        }
        let Some(items) = d.items else { return };
        match (d.cancelled, d.finished) {
            (_, Some(true)) => {}
            (true, _) if !d.released => discard(items),
            _ => {
                let inner = self.inner.clone();
                let name = self.peer_name(&d.peer);
                std::thread::spawn(move || {
                    let Some(dest) = files::downloads_dir() else { return };
                    match files::keep_in(&items, &dest) {
                        Ok(kept) => {
                            tracing::info!(count = kept.len(), "desk: dragged files kept in Downloads/My Computer");
                            if let Some(first) = kept.first() {
                                inner.host.reveal(first);
                            }
                        }
                        Err(e) => tracing::warn!(from = name.as_str(), error = e.as_str(), "desk: could not keep the dragged files"),
                    }
                });
            }
        }
    }
}

/// Deletes the staging folder `items` were received into.
pub(super) fn discard(items: Vec<PathBuf>) {
    let root = files::staging_root();
    if let Some(stage) = items.first().and_then(|p| p.parent()).map(|p| p.to_path_buf()) {
        if stage.starts_with(&root) && stage != root {
            std::thread::spawn(move || {
                let _ = std::fs::remove_dir_all(stage);
            });
        }
    }
}
