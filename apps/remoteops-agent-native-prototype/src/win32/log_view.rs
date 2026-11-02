//! 标准多行 EDIT 的分块增量呈现；控件与模型统一使用 UTF-16 / CRLF 索引。

use super::{get_text, set_text, wide};
use crate::{
    model::{RenderCursor, TextPatch},
    render_chunk::chunk_end,
};
use std::{
    cell::{Cell, RefCell},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow},
    UI::{
        Controls::{EM_EMPTYUNDOBUFFER, EM_REPLACESEL, EM_SCROLLCARET, EM_SETSEL},
        WindowsAndMessaging::{IsWindowVisible, SendMessageW, WM_SETREDRAW},
    },
};

pub(super) const MAX_APPEND_UNITS: usize = 512;
const MAX_APPEND_LINES: usize = 32;
const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(33);

struct PendingRender {
    // 只保存有界模型的一次呈现目标；新目标到来可取代尚未显示的中间帧。
    cursor: RenderCursor,
    units: Vec<u16>,
    offset: usize,
}

#[derive(Default)]
pub(super) struct LogView {
    cursor: Cell<Option<RenderCursor>>,
    pending: RefCell<Option<PendingRender>>,
    full_replacements: Cell<u64>,
    incremental_updates: Cell<u64>,
    coalesced_updates: Cell<u64>,
    peak_append_units: Cell<usize>,
    phase_max_ms: Cell<[u128; 3]>,
    append_max_ms: Cell<[u128; 2]>,
    last_update: Cell<Option<Instant>>,
}

impl LogView {
    pub(super) fn cursor(&self) -> Option<RenderCursor> {
        self.cursor.get()
    }
    pub(super) fn reset_window(&self) {
        self.cursor.set(None);
        self.pending.borrow_mut().take();
        self.last_update.set(None);
    }
    pub(super) fn counters(&self) -> (u64, u64) {
        (self.full_replacements.get(), self.incremental_updates.get())
    }
    pub(super) fn phase_max_ms(&self) -> [u128; 3] {
        self.phase_max_ms.get()
    }
    pub(super) fn append_max_ms(&self) -> [u128; 2] {
        self.append_max_ms.get()
    }
    pub(super) fn deferred_metrics(&self) -> (u64, usize, usize) {
        let remaining = self
            .pending
            .borrow()
            .as_ref()
            .map_or(0, |pending| pending.units.len() - pending.offset);
        (
            self.coalesced_updates.get(),
            self.peak_append_units.get(),
            remaining,
        )
    }

    fn start_target(&self, terminal: HWND, patch: &TextPatch, previous: Option<RenderCursor>) {
        if patch.replace_all {
            // 完整同步也分块追加，不在消息线程一次排版整个历史。
            set_text(terminal, "");
            self.full_replacements.set(self.full_replacements.get() + 1);
        } else {
            debug_assert!(previous.is_some(), "增量补丁需要已完成的游标");
            let removed = patch.remove_utf16_units;
            if removed > 0 {
                unsafe {
                    SendMessageW(terminal, EM_SETSEL, 0, removed as isize);
                    SendMessageW(terminal, EM_REPLACESEL, 0, wide("").as_ptr() as isize);
                }
            }
        }
        *self.pending.borrow_mut() = Some(PendingRender {
            cursor: patch.cursor,
            units: patch.append.encode_utf16().collect(),
            offset: 0,
        });
        self.cursor.set(None);
    }

    fn next_chunk(&self) -> (Vec<u16>, usize, bool) {
        let pending = self.pending.borrow();
        let pending = pending.as_ref().expect("呈现目标已建立");
        let end = chunk_end(
            &pending.units,
            pending.offset,
            MAX_APPEND_UNITS,
            MAX_APPEND_LINES,
        );
        (
            pending.units[pending.offset..end].to_vec(),
            end,
            end == pending.units.len(),
        )
    }

    fn append_chunk(&self, terminal: HWND, mut chunk: Vec<u16>) {
        if chunk.is_empty() {
            return;
        }
        self.peak_append_units
            .set(self.peak_append_units.get().max(chunk.len()));
        chunk.push(0);
        let selection = Instant::now();
        unsafe { SendMessageW(terminal, EM_SETSEL, i32::MAX as usize, i32::MAX as isize) };
        let mut append_max = self.append_max_ms.get();
        append_max[0] = append_max[0].max(selection.elapsed().as_millis());
        let insert = Instant::now();
        unsafe { SendMessageW(terminal, EM_REPLACESEL, 0, chunk.as_ptr() as isize) };
        append_max[1] = append_max[1].max(insert.elapsed().as_millis());
        self.append_max_ms.set(append_max);
        self.incremental_updates
            .set(self.incremental_updates.get() + 1);
    }

    pub(super) fn apply(&self, terminal: HWND, patch: &TextPatch) {
        self.apply_streaming(terminal, patch, false);
    }

    pub(super) fn apply_streaming(&self, terminal: HWND, patch: &TextPatch, streaming: bool) {
        if terminal.is_null() || self.cursor.get() == Some(patch.cursor) {
            return;
        }
        let changed_target = self
            .pending
            .borrow()
            .as_ref()
            .is_none_or(|pending| pending.cursor != patch.cursor);
        // 连续输入时合并过密的中间帧；同一目标的剩余片段不节流，停流后立即追赶。
        if streaming
            && changed_target
            && self
                .last_update
                .get()
                .is_some_and(|last| last.elapsed() < STREAM_FRAME_INTERVAL)
        {
            return;
        }
        let previous = self.cursor.get();
        let replaced_pending = if changed_target {
            self.pending.borrow_mut().take()
        } else {
            None
        };
        if replaced_pending.is_some() {
            self.coalesced_updates.set(self.coalesced_updates.get() + 1);
        }
        if replaced_pending.is_some() && !patch.replace_all {
            // 部分呈现后旧游标已失效；要求下一轮从模型重新生成完整目标。
            self.cursor.set(None);
            return;
        }
        let visible = unsafe { IsWindowVisible(terminal) } != 0;
        // 只有可见控件配对关闭/恢复重绘，避免给隐藏观察窗意外增加 WS_VISIBLE。
        if visible {
            unsafe { SendMessageW(terminal, WM_SETREDRAW, 0, 0) };
        }
        let phase = Instant::now();
        let mut maxima = self.phase_max_ms.get();
        if changed_target {
            self.start_target(terminal, patch, previous);
        }
        maxima[0] = maxima[0].max(phase.elapsed().as_millis());
        let phase = Instant::now();
        // 不跨同步窗口调用持有 RefCell 借用，防止重入消息再次访问呈现状态。
        let (chunk, offset, complete) = self.next_chunk();
        self.append_chunk(terminal, chunk);
        if complete {
            self.pending.borrow_mut().take();
            self.cursor.set(Some(patch.cursor));
        } else if let Some(pending) = self.pending.borrow_mut().as_mut() {
            pending.offset = offset;
        }
        maxima[1] = maxima[1].max(phase.elapsed().as_millis());
        let phase = Instant::now();
        unsafe {
            SendMessageW(terminal, EM_SETSEL, i32::MAX as usize, i32::MAX as isize);
            SendMessageW(terminal, EM_SCROLLCARET, 0, 0);
            if visible {
                SendMessageW(terminal, WM_SETREDRAW, 1, 0);
                RedrawWindow(
                    terminal,
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_ERASE | RDW_FRAME,
                );
            }
        }
        maxima[2] = maxima[2].max(phase.elapsed().as_millis());
        self.phase_max_ms.set(maxima);
        self.last_update.set(Some(Instant::now()));
    }
}

pub(super) fn disable_undo(terminal: HWND) {
    unsafe { SendMessageW(terminal, EM_EMPTYUNDOBUFFER, 0, 0) };
}

pub(super) fn text(terminal: HWND) -> String {
    get_text(terminal)
}
