//! 连接存活检测。
//!
//! 不用 russh 自带的 keepalive：它的计时器与写 socket 在同一个循环里，对端无响应、发送缓冲写满时
//! 循环卡在写入上，计时器得不到执行；连接结束时它也不唤醒等待通道窗口的写入方。
//!
//! 这里在传输流外包一层 [`Guarded`]：记录最后收到数据的时间，看门狗据此定时探测，判定无响应时
//! 让流的读写立即报错，迫使 russh 主循环退出。传输流被释放（主循环以任何原因结束）即发出断开信号，
//! 挂在同一信号上的通道、SFTP、转发的读写随之返回，不会停在等窗口上。

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, ready};
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use russh::client::Handle;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Instant;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use crate::connect::ClientHandler;

/// 墙钟比单调时钟多走这么多，视为系统挂起过。
const SUSPEND_GAP: Duration = Duration::from_secs(5);

pub(crate) struct Liveness {
    last_read: Mutex<Instant>,
    dead: CancellationToken,
    timed_out: AtomicBool,
}

impl Liveness {
    pub(crate) fn new(dead: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            last_read: Mutex::new(Instant::now()),
            dead,
            timed_out: AtomicBool::new(false),
        })
    }

    pub(crate) fn token(&self) -> &CancellationToken {
        &self.dead
    }

    pub(crate) fn is_dead(&self) -> bool {
        self.dead.is_cancelled()
    }

    /// 距上次从对端收到数据的时长。
    pub(crate) fn idle(&self) -> Duration {
        self.last_read.lock().elapsed()
    }

    /// 判定对端无响应并断开。
    pub(crate) fn time_out(&self) {
        self.timed_out.store(true, Ordering::Relaxed);
        self.dead.cancel();
    }

    pub(crate) fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::Relaxed)
    }
}

/// 断开信号触发后读写立即报错的流。
///
/// 作为连接的传输流时还负责记录收到数据的时间，并在读到结尾、出错或被释放时发出断开信号；
/// 作为通道流时只跟随信号，通道自身关闭不影响连接。
pub struct Guarded<S> {
    inner: S,
    dead: CancellationToken,
    // 读写可能由不同任务驱动，各自登记唤醒
    read_dead: Pin<Box<WaitForCancellationFutureOwned>>,
    write_dead: Pin<Box<WaitForCancellationFutureOwned>>,
    transport: Option<Arc<Liveness>>,
}

impl<S> Guarded<S> {
    pub(crate) fn transport(inner: S, liveness: Arc<Liveness>) -> Self {
        let mut this = Self::channel(inner, liveness.token());
        this.transport = Some(liveness);
        this
    }

    pub(crate) fn channel(inner: S, dead: &CancellationToken) -> Self {
        Self {
            inner,
            dead: dead.clone(),
            read_dead: Box::pin(dead.clone().cancelled_owned()),
            write_dead: Box::pin(dead.clone().cancelled_owned()),
            transport: None,
        }
    }

    fn end_transport(&self) {
        if let Some(l) = &self.transport {
            l.dead.cancel();
        }
    }
}

fn aborted() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "SSH connection closed")
}

/// 断开信号已触发时返回 `true`，否则登记唤醒。
fn poll_dead(
    dead: &CancellationToken,
    fut: &mut Pin<Box<WaitForCancellationFutureOwned>>,
    cx: &mut Context<'_>,
) -> bool {
    dead.is_cancelled() || fut.as_mut().poll(cx).is_ready()
}

impl<S: AsyncRead + Unpin> AsyncRead for Guarded<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if poll_dead(&this.dead, &mut this.read_dead, cx) {
            return Poll::Ready(Err(aborted()));
        }
        let before = buf.filled().len();
        let result = ready!(Pin::new(&mut this.inner).poll_read(cx, buf));
        match (&result, &this.transport) {
            (Ok(()), Some(l)) if buf.filled().len() > before => {
                *l.last_read.lock() = Instant::now()
            }
            _ => this.end_transport(),
        }
        Poll::Ready(result)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Guarded<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if poll_dead(&this.dead, &mut this.write_dead, cx) {
            return Poll::Ready(Err(aborted()));
        }
        let result = ready!(Pin::new(&mut this.inner).poll_write(cx, buf));
        if result.is_err() {
            this.end_transport();
        }
        Poll::Ready(result)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if poll_dead(&this.dead, &mut this.write_dead, cx) {
            return Poll::Ready(Err(aborted()));
        }
        let result = ready!(Pin::new(&mut this.inner).poll_flush(cx));
        if result.is_err() {
            this.end_transport();
        }
        Poll::Ready(result)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if poll_dead(&this.dead, &mut this.write_dead, cx) {
            return Poll::Ready(Err(aborted()));
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

impl<S> Drop for Guarded<S> {
    fn drop(&mut self) {
        self.end_transport();
    }
}

/// 看门狗：连续 `interval` 收不到任何数据就发一次 keepalive 探测，连发 `max` 次都没有回音即判定无响应。
/// 系统挂起后醒来立即探测，一个周期内没有回音即判定断开（NAT 映射多半已在挂起期间失效）。
pub(crate) fn spawn_watchdog(
    handle: Weak<Handle<ClientHandler>>,
    liveness: Arc<Liveness>,
    interval: Duration,
    max: u32,
) {
    let tick = (interval / 4).clamp(Duration::from_millis(100), Duration::from_secs(1));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut probe_at: Option<Instant> = None;
        let mut unanswered = 0;
        let mut clock = (Instant::now(), SystemTime::now());
        loop {
            tokio::select! {
                () = liveness.dead.cancelled() => return,
                _ = ticker.tick() => {}
            }
            let Some(handle) = handle.upgrade() else {
                return;
            };
            let now = Instant::now();
            let last_read = *liveness.last_read.lock();
            if probe_at.is_none_or(|p| last_read >= p) {
                unanswered = 0;
            }
            let wall = SystemTime::now();
            let wall_elapsed = wall.duration_since(clock.1).unwrap_or_default();
            let resumed = wall_elapsed > (now - clock.0) + SUSPEND_GAP;
            clock = (now, wall);

            let quiet = now - probe_at.map_or(last_read, |p| p.max(last_read));
            if resumed {
                tracing::debug!("系统挂起后恢复，探测连接");
                unanswered = max;
            } else if quiet >= interval {
                if unanswered >= max {
                    tracing::info!(idle = ?liveness.idle(), "SSH 连接无响应，断开");
                    liveness.time_out();
                    return;
                }
                unanswered += 1;
            } else {
                continue;
            }
            probe_at = Some(now);
            // 主循环卡住时发送队列可能已满，不能在这里一直等
            let _ = tokio::time::timeout(tick, handle.send_keepalive(true)).await;
        }
    });
}
