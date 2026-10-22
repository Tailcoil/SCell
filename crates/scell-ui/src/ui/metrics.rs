//! 全局尺寸。间距与字号用组件库按 rem 计的方法（随界面字号缩放），这里只放需要固定像素的结构尺寸。

use gpui_kit::{Pixels, Size, px, size};

/// 窗口的最小尺寸。
pub(crate) const WINDOW_MIN_SIZE: Size<Pixels> = size(px(720.), px(480.));

/// 左侧栏展开与收起后的宽度。
pub(crate) const SIDEBAR_WIDTH: Pixels = px(240.);
pub(crate) const SIDEBAR_COLLAPSED_WIDTH: Pixels = px(48.);
/// 侧栏与列表中一行的高度。
pub(crate) const LIST_ROW_HEIGHT: Pixels = px(32.);
/// 两行（名称 + 地址）列表项的高度。
pub(crate) const LIST_ROW_TALL_HEIGHT: Pixels = px(44.);

/// 右侧上下文面板的默认宽度与可调范围。
pub(crate) const CONTEXT_PANEL_WIDTH: Pixels = px(280.);
/// 内容区的最小宽度：放下右侧面板后不足此宽时面板临时收起，设置页的表单也按此宽度能完整放下。
pub(crate) const CONTENT_MIN_WIDTH: Pixels = px(360.);
pub(crate) const CONTEXT_PANEL_MIN: Pixels = px(200.);
pub(crate) const CONTEXT_PANEL_MAX: Pixels = px(480.);

/// 底部面板（传输）的默认高度与可调范围。
pub(crate) const BOTTOM_PANEL_HEIGHT: Pixels = px(220.);
pub(crate) const BOTTOM_PANEL_MIN: Pixels = px(120.);
pub(crate) const BOTTOM_PANEL_MAX: Pixels = px(480.);

/// 页面（设置等）内容列的最大宽度：单栏表单与左右两栏。
pub(crate) const PAGE_WIDTH: Pixels = px(640.);
pub(crate) const PAGE_WIDE_WIDTH: Pixels = px(1000.);
/// 页面左右留白，也是页内各栏之间的间距（与 `px_6` / `gap_6` 一致）。
pub(crate) const PAGE_GUTTER: Pixels = px(24.);
/// 页内主从布局左侧列表的宽度。
pub(crate) const MASTER_LIST_WIDTH: Pixels = px(220.);
/// 主机编辑器左栏（名称、分组、图标）的宽度，与右栏（分页内容）的最小宽度。
pub(crate) const EDITOR_SIDE_WIDTH: Pixels = px(184.);
pub(crate) const EDITOR_MAIN_MIN_WIDTH: Pixels = px(300.);
/// 内容区宽于此值时主机列表与编辑器并排，否则单栏切换。
/// 约为页面左右留白、列表、间距与编辑器两栏最小宽度之和。
pub(crate) const HOSTS_SPLIT_MIN: Pixels = px(820.);

/// 表单行内控件的标准宽度，保持各行右缘对齐。
pub(crate) const CONTROL_WIDTH: Pixels = px(240.);
/// “主机”输入框的基准宽度，比一般字段小，使“连接 / 主机 / 端口”在窄栏中也排在一行。
pub(crate) const HOST_FIELD_BASIS: Pixels = px(120.);
/// 转发规则中监听框的宽度与目标框的基准宽度，使“类型 / 监听 → 目标”在编辑器右栏的最小宽度内排成一行。
pub(crate) const FORWARD_LISTEN_WIDTH: Pixels = px(64.);
pub(crate) const FORWARD_TARGET_BASIS: Pixels = px(96.);
/// 端口输入框的宽度。
pub(crate) const PORT_WIDTH: Pixels = px(80.);
/// 数值输入框的宽度。
pub(crate) const NUMBER_WIDTH: Pixels = px(132.);
/// 表单行的最小高度。
pub(crate) const FORM_ROW_HEIGHT: Pixels = px(48.);

/// 文件列表一行的高度，以及大小、修改时间两列的宽度。
pub(crate) const FILE_ROW_HEIGHT: Pixels = px(28.);
pub(crate) const FILE_SIZE_WIDTH: Pixels = px(64.);
pub(crate) const FILE_TIME_WIDTH: Pixels = px(96.);
/// 文件列表宽于此值时才显示修改时间列，名称列至少留下约 160px。
pub(crate) const FILE_TIME_MIN_WIDTH: Pixels = px(400.);

/// 传输弹出列表的宽度。
pub(crate) const TRANSFER_LIST_WIDTH: Pixels = px(340.);

/// 右侧滑出的编辑 Sheet 的宽度。
pub(crate) const SHEET_WIDTH: Pixels = px(640.);
/// 命令面板的宽度与距窗口顶部的距离。
pub(crate) const PALETTE_WIDTH: Pixels = px(520.);
pub(crate) const PALETTE_TOP: Pixels = px(72.);

/// 分屏中窗格的最小边长。
pub(crate) const PANE_MIN: Pixels = px(64.);
/// 窗格卡片之间的留白与卡片内边距。
pub(crate) const PANE_GAP: Pixels = px(8.);
pub(crate) const PANE_PADDING: Pixels = px(6.);

/// 状态点（环境色、已连接、角标）的直径。
pub(crate) const DOT: Pixels = px(6.);

/// 窗格内提示卡片（密码、主机密钥等）的宽度。
pub(crate) const PROMPT_WIDTH: Pixels = px(360.);
/// 查找条输入框的宽度。
pub(crate) const FIND_INPUT_WIDTH: Pixels = px(200.);
/// 窗口外框按钮（最小化等）的边长。
pub(crate) const CHROME_BUTTON: Pixels = px(28.);
/// 上下文面板没有会话可跟随时，居中的淡色标记的边长。
pub(crate) const EMPTY_MARK: Pixels = px(56.);
/// 设置页的分页标签全部放下所需的内容区宽度，更窄时标签栏带溢出菜单。
pub(crate) const SETTINGS_TABS_MIN: Pixels = px(460.);
/// 设置“关于”页的应用图标边长。
pub(crate) const ABOUT_LOGO: Pixels = px(88.);
/// 浮层出现时的位移距离。
pub(crate) const ENTER_OFFSET: Pixels = px(8.);
