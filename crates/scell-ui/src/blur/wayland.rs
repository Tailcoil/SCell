//! 在 GPUI 的 Wayland 连接上另开一个事件队列，为窗口 surface 设置模糊区域。
//!
//! 优先用标准的 ext-background-effect（KWin 6.8 起只提供它），否则用 KDE 的旧协议。
//! 两者都按可见窗口框和它的圆角设置区域：GPUI 自己申请模糊时整个 surface 都会被模糊，
//! 包括四周画阴影的透明边距，所以 Wayland 上不再让 GPUI 申请（见 `appearance::background_appearance`）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::ManuallyDrop;

use gpui_kit::component::{window_corner_radii, window_paddings};
use gpui_kit::{Window, WindowId};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_compositor, wl_region, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1 as ext_manager, ext_background_effect_surface_v1 as ext_effect,
};
use wayland_protocols_plasma::blur::client::{
    org_kde_kwin_blur as kde_blur, org_kde_kwin_blur_manager as kde_manager,
};

struct Client {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    compositor: wl_compositor::WlCompositor,
    manager: Manager,
    /// 区域坐标相对窗口几何而非 surface。KWin 如此解释（新旧两个协议都是），与协议规定的 surface 坐标不符：
    /// GPUI 把窗口几何设为去掉阴影边距的窗口框，在 KWin 上按 surface 坐标给出会整体偏移一个边距。
    frame_relative: bool,
    windows: HashMap<WindowId, Applied>,
}

enum Manager {
    Ext(ext_manager::ExtBackgroundEffectManagerV1),
    Kde(kde_manager::OrgKdeKwinBlurManager),
}

enum Effect {
    Ext(ext_effect::ExtBackgroundEffectSurfaceV1),
    Kde(kde_blur::OrgKdeKwinBlur),
}

struct State {
    /// 合成器当前能否模糊。新协议在用户关掉模糊特效时会发来新的 capabilities；旧协议的管理器存在即表示可以。
    blur: bool,
}

struct Applied {
    surface: wl_surface::WlSurface,
    effect: Option<Effect>,
    /// 上次设置的区域；空表示未模糊。
    region: Vec<[i32; 4]>,
}

thread_local! {
    /// 外层 `None` 为尚未初始化，内层 `None` 为不可用（非 Wayland 或合成器不支持任何模糊协议）。
    /// 不析构：退出时 GPUI 先断开显示连接，之后再销毁这里的事件队列会访问已释放的连接。
    static CLIENT: RefCell<Option<ManuallyDrop<Option<Client>>>> = const { RefCell::new(None) };
}

pub(super) fn sync(window: &Window, blur: bool) {
    CLIENT.with_borrow_mut(|client| {
        let client = client.get_or_insert_with(|| ManuallyDrop::new(connect(window)));
        if let Some(client) = client.as_mut() {
            client.sync(window, blur);
        }
    });
}

pub(super) fn forget(window: WindowId) {
    CLIENT.with_borrow_mut(|client| {
        if let Some(Some(client)) = client.as_deref_mut()
            && let Some(applied) = client.windows.remove(&window)
        {
            match applied.effect {
                Some(Effect::Ext(effect)) => effect.destroy(),
                Some(Effect::Kde(blur)) => blur.release(),
                None => {}
            }
            let _ = client.conn.flush();
        }
    });
}

fn connect(window: &Window) -> Option<Client> {
    let RawDisplayHandle::Wayland(handle) = window.display_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: 指针来自 GPUI 正在使用的 wl_display，存活到进程结束（见 CLIENT）；
    // 外部显示连接由 GPUI 负责读写，这里只在主线程上另建一个事件队列
    #[allow(unsafe_code)]
    let backend = unsafe { Backend::from_foreign_display(handle.display.as_ptr().cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
    let qh = queue.handle();
    let frame_relative = globals
        .contents()
        .with_list(|list| list.iter().any(|g| g.interface.starts_with("org_kde_")));
    let compositor = globals.bind(&qh, 1..=4, ()).ok()?;
    let mut state = State { blur: false };
    let manager = bind_manager(&globals, &qh, &mut state)?;
    // 新协议绑定后立即发来 capabilities
    queue.roundtrip(&mut state).ok()?;
    tracing::debug!(
        protocol = match manager {
            Manager::Ext(_) => "ext-background-effect",
            Manager::Kde(_) => "org_kde_kwin_blur",
        },
        blur = state.blur,
        "窗口模糊"
    );
    Some(Client {
        conn,
        queue,
        state,
        compositor,
        manager,
        frame_relative,
        windows: HashMap::new(),
    })
}

fn bind_manager(
    globals: &GlobalList,
    qh: &QueueHandle<State>,
    state: &mut State,
) -> Option<Manager> {
    if let Ok(manager) = globals.bind(qh, 1..=1, ()) {
        return Some(Manager::Ext(manager));
    }
    let manager = globals.bind(qh, 1..=1, ()).ok()?;
    state.blur = true;
    Some(Manager::Kde(manager))
}

impl Client {
    fn sync(&mut self, window: &Window, blur: bool) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        let wanted = if blur && self.state.blur {
            frame_region(window, self.frame_relative)
        } else {
            Vec::new()
        };
        let id = window.window_handle().window_id();
        if !self.windows.contains_key(&id) {
            if wanted.is_empty() {
                return;
            }
            let Some(surface) = surface(&self.conn, window) else {
                return;
            };
            self.windows.insert(
                id,
                Applied {
                    surface,
                    effect: None,
                    region: Vec::new(),
                },
            );
        }
        let Some(applied) = self.windows.get_mut(&id) else {
            return;
        };
        if applied.region == wanted {
            return;
        }
        let qh = self.queue.handle();
        let region = (!wanted.is_empty()).then(|| {
            let region = self.compositor.create_region(&qh, ());
            for [x, y, w, h] in &wanted {
                region.add(*x, *y, *w, *h);
            }
            region
        });
        match &self.manager {
            Manager::Ext(manager) => {
                let effect = applied.effect.get_or_insert_with(|| {
                    Effect::Ext(manager.get_background_effect(&applied.surface, &qh, ()))
                });
                if let Effect::Ext(effect) = effect {
                    effect.set_blur_region(region.as_ref());
                }
            }
            Manager::Kde(manager) => match &region {
                Some(region) => {
                    let effect = applied.effect.get_or_insert_with(|| {
                        Effect::Kde(manager.create(&applied.surface, &qh, ()))
                    });
                    if let Effect::Kde(blur) = effect {
                        blur.set_region(Some(region));
                        blur.commit();
                    }
                }
                // 旧协议的空区域表示整个 surface，取消模糊要用 unset
                None => {
                    manager.unset(&applied.surface);
                    if let Some(Effect::Kde(blur)) = applied.effect.take() {
                        blur.release();
                    }
                }
            },
        }
        if let Some(region) = region {
            region.destroy();
        }
        applied.region = wanted;
        // 区域是双缓冲状态，在 GPUI 下次提交这一帧时生效
        let _ = self.conn.flush();
    }
}

fn surface(conn: &Connection, window: &Window) -> Option<wl_surface::WlSurface> {
    // Window 自带同名的 window_handle()（返回 GPUI 的窗口句柄），这里要的是 raw-window-handle 的
    let RawWindowHandle::Wayland(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    // SAFETY: 指针是 GPUI 为该窗口持有的 wl_surface，窗口关闭前一直有效；关闭后由 forget 释放效果对象
    #[allow(unsafe_code)]
    let id = unsafe {
        ObjectId::from_ptr(
            wl_surface::WlSurface::interface(),
            handle.surface.as_ptr().cast(),
        )
    }
    .ok()?;
    wl_surface::WlSurface::from_id(conn, id).ok()
}

/// 可见窗口框（不含四周画阴影的透明边距）按圆角拼成的矩形；`frame_relative` 见 [`Client::frame_relative`]。
fn frame_region(window: &Window, frame_relative: bool) -> Vec<[i32; 4]> {
    let size = window.viewport_size();
    let pad = window_paddings(window);
    let radii = window_corner_radii(window);
    let (x, y) = if frame_relative {
        (0., 0.)
    } else {
        (pad.left.as_f32(), pad.top.as_f32())
    };
    let w = (size.width - pad.left - pad.right).as_f32();
    let h = (size.height - pad.top - pad.bottom).as_f32();
    rounded_rects(
        [x, y, w, h],
        [
            radii.top_left.as_f32(),
            radii.top_right.as_f32(),
            radii.bottom_left.as_f32(),
            radii.bottom_right.as_f32(),
        ],
    )
}

/// 把圆角矩形近似成逐行收进的矩形（wl_region 只能由矩形拼成），相邻同宽的行合并。
/// `radii` 依次为左上、右上、左下、右下。
fn rounded_rects([x, y, w, h]: [f32; 4], [tl, tr, bl, br]: [f32; 4]) -> Vec<[i32; 4]> {
    let (x, y, w, h) = (
        x.round() as i32,
        y.round() as i32,
        w.round() as i32,
        h.round() as i32,
    );
    if w <= 0 || h <= 0 {
        return Vec::new();
    }
    // 第 row 行（从该角所在的边数起）在半径 r 的圆角处应收进的像素数
    let inset = |r: f32, row: i32| -> i32 {
        let r = r.min(w as f32 / 2.).min(h as f32 / 2.);
        if row as f32 >= r {
            return 0;
        }
        let dy = r - row as f32 - 0.5;
        (r - (r * r - dy * dy).max(0.).sqrt()).round() as i32
    };
    let mut rows: Vec<[i32; 4]> = Vec::new();
    let mut push = |left: i32, right: i32, top: i32| {
        let rect = [x + left, y + top, w - left - right, 1];
        match rows.last_mut() {
            Some(last)
                if last[0] == rect[0] && last[2] == rect[2] && last[1] + last[3] == rect[1] =>
            {
                last[3] += 1;
            }
            _ => rows.push(rect),
        }
    };
    let top_band = tl.max(tr).ceil() as i32;
    let bottom_band = bl.max(br).ceil() as i32;
    for row in 0..h {
        let from_bottom = h - 1 - row;
        let (left, right) = if row < top_band {
            (inset(tl, row), inset(tr, row))
        } else if from_bottom < bottom_band {
            (inset(bl, from_bottom), inset(br, from_bottom))
        } else {
            (0, 0)
        };
        push(left, right, row);
    }
    rows
}

impl Dispatch<ext_manager::ExtBackgroundEffectManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_manager::ExtBackgroundEffectManagerV1,
        event: ext_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_manager::Event::Capabilities { flags } = event {
            state.blur =
                matches!(flags, WEnum::Value(f) if f.contains(ext_manager::Capability::Blur));
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_region::WlRegion);
delegate_noop!(State: ignore ext_effect::ExtBackgroundEffectSurfaceV1);
delegate_noop!(State: ignore kde_manager::OrgKdeKwinBlurManager);
delegate_noop!(State: ignore kde_blur::OrgKdeKwinBlur);

#[cfg(test)]
mod tests {
    use super::rounded_rects;

    #[test]
    fn square_frame_is_one_rect() {
        assert_eq!(
            rounded_rects([20., 20., 100., 50.], [0.; 4]),
            [[20, 20, 100, 50]]
        );
    }

    #[test]
    fn rounded_corners_step_inwards_and_cover_every_row() {
        let rects = rounded_rects([0., 0., 100., 60.], [10.; 4]);
        // 第一行收进最多，中间是一整块（圆角末端收进不足半像素的行也并入），上下对称
        assert!(rects[0][0] > 5 && rects[0][0] + rects[0][2] < 95);
        assert!(rects.iter().any(|r| r[0] == 0 && r[2] == 100 && r[3] >= 40));
        assert_eq!(rects.first().map(|r| r[0]), rects.last().map(|r| r[0]));
        assert_eq!(rects.iter().map(|r| r[3]).sum::<i32>(), 60);
    }
}
