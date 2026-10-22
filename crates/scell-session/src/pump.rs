use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use scell_term::TermEvent;
use tokio::runtime::Handle;

use crate::SessionEvent;
use crate::codec::Decoder;
use crate::session::Shared;

/// 输出方向的数据泵：解码 → 喂给终端 → 分发事件 → 合并唤醒。
pub(crate) struct Pump {
    shared: Arc<Shared>,
    decoder: Decoder,
    events: Vec<TermEvent>,
    sync: Arc<SyncTimer>,
}

/// DEC 2026 同步更新超时计时器。读线程阻塞在 read 上无法自行感知超时，交给运行时定时刷新。
struct SyncTimer {
    rt: Handle,
    armed: AtomicBool,
}

impl Pump {
    pub(crate) fn new(shared: Arc<Shared>, decoder: Decoder, rt: Handle) -> Self {
        Self {
            shared,
            decoder,
            events: Vec::new(),
            sync: Arc::new(SyncTimer {
                rt,
                armed: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn on_output(&mut self, bytes: &[u8]) {
        let utf8 = self.decoder.decode(bytes);
        self.shared.terminal.feed(utf8, &mut self.events);
        dispatch(&self.shared, &mut self.events);
        self.shared.wakeup();
        self.arm_sync_timer();
    }

    fn arm_sync_timer(&self) {
        let Some(deadline) = self.shared.terminal.sync_deadline() else {
            return;
        };
        if self.sync.armed.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = self.shared.clone();
        let sync = self.sync.clone();
        self.sync.rt.spawn(async move {
            tokio::time::sleep_until(deadline.into()).await;
            sync.armed.store(false, Ordering::Release);
            let mut events = Vec::new();
            shared.terminal.flush_sync(&mut events);
            dispatch(&shared, &mut events);
            shared.wakeup();
        });
    }
}

/// 把会话自身的提示（连接进度、错误）直接写进终端，不经过远端编码的解码器。
pub(crate) fn notice(shared: &Shared, text: &str) {
    let mut events = Vec::new();
    shared.terminal.feed(text.as_bytes(), &mut events);
    dispatch(shared, &mut events);
    shared.wakeup();
}

fn dispatch(shared: &Shared, events: &mut Vec<TermEvent>) {
    let id = shared.id;
    for event in events.drain(..) {
        match event {
            TermEvent::PtyWrite(bytes) => {
                let _ = shared.input.send(bytes);
            }
            TermEvent::Title(t) => shared.emit(SessionEvent::Title(id, Some(t))),
            TermEvent::ResetTitle => shared.emit(SessionEvent::Title(id, None)),
            TermEvent::Bell => shared.emit(SessionEvent::Bell(id)),
            TermEvent::WorkingDirectory(url) => {
                shared.emit(SessionEvent::WorkingDirectory(id, url));
            }
            TermEvent::ClipboardStore(_, text) => {
                shared.emit(SessionEvent::ClipboardStore(id, text));
            }
            _ => {}
        }
    }
}
