use std::time::Instant;

use gpui_kit::component::input::InputState;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::{Entity, SharedString, Subscription};
use rust_i18n::t;
use scell_config::HostId;
use scell_session::{LocalShell, Prompt, PromptAnswer, PromptKind, SessionHandle, SessionId};
use scell_term::TermSize;
use scell_term_view::{TerminalIo, TerminalView};

use super::pane_tree::{PaneId, PaneTree};
use crate::files::SftpView;
use crate::tabs::SettingsView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TabId(pub(crate) u64);

/// 窗格的启动方式，重启与分屏时据此再开一个同样的会话。
#[derive(Debug, Clone)]
pub(crate) enum Launch {
    Local(LocalShell),
    Ssh { host: HostId, name: SharedString },
}

/// 会话等待用户回答的问题，以卡片形式显示在窗格内。
pub(crate) struct PanePrompt {
    pub(crate) prompt: Prompt,
    /// 需要输入的问题各对应一个输入框；确认类问题为空。
    pub(crate) inputs: Vec<Entity<InputState>>,
    /// 密码与口令卡片上的“记住”。
    pub(crate) remember: bool,
    pub(crate) _events: Vec<Subscription>,
}

impl PanePrompt {
    /// 是否是可以存入凭据库的问题。
    pub(crate) fn rememberable(&self) -> bool {
        matches!(
            self.prompt.kind,
            PromptKind::Password { .. } | PromptKind::Passphrase { .. }
        )
    }

    /// 按输入框内容组成回答；没有输入框时为“确认”。
    pub(crate) fn submission(&self, cx: &gpui_kit::App) -> PromptAnswer {
        let mut values = self.inputs.iter().map(|i| i.read(cx).value().to_string());
        match &self.prompt.kind {
            PromptKind::Interactive { .. } => PromptAnswer::Responses(values.collect()),
            _ => match values.next() {
                Some(value) => PromptAnswer::Secret {
                    value,
                    remember: self.remember,
                },
                None => PromptAnswer::Accept,
            },
        }
    }
}

/// 窗格里会话的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneStatus {
    /// 本地进程在运行，或远程会话已连上。
    Running,
    /// 远程会话正在（重新）连接。
    Connecting,
    /// 远程连接已断开；`retry_at` 为自动重连的时间。
    Disconnected { retry_at: Option<Instant> },
    /// 进程已退出，附退出码（被信号终止时为 `None`）。
    Exited(Option<u32>),
    /// 恢复出来的远程会话，还没有连接。
    Idle,
}

impl PaneStatus {
    /// 会话已不再交互，窗格显示重启/重连横幅。
    pub(crate) fn ended(self) -> bool {
        matches!(
            self,
            Self::Disconnected { .. } | Self::Exited(_) | Self::Idle
        )
    }
}

/// 一个终端窗格：会话、视图与其状态。
pub(crate) struct Pane {
    pub(crate) id: PaneId,
    pub(crate) session: SessionHandle,
    pub(crate) view: Entity<TerminalView>,
    pub(crate) launch: Launch,
    /// 程序通过 OSC 0/2 设置的标题。
    pub(crate) title: Option<String>,
    pub(crate) status: PaneStatus,
    /// 程序通过 OSC 7 上报的本机工作目录。
    pub(crate) cwd: Option<std::path::PathBuf>,
    /// OSC 7 上报的目录，原始字节，不论在哪台主机上。
    pub(crate) osc_cwd: Option<scell_vfs::VfsPath>,
    /// 不可见时响过铃，聚焦后清除。
    pub(crate) bell: bool,
    pub(crate) prompt: Option<PanePrompt>,
}

impl Pane {
    pub(crate) fn new(
        id: PaneId,
        session: SessionHandle,
        view: Entity<TerminalView>,
        launch: Launch,
    ) -> Self {
        Self {
            id,
            session,
            view,
            launch,
            title: None,
            status: PaneStatus::Running,
            cwd: None,
            osc_cwd: None,
            bell: false,
            prompt: None,
        }
    }
}

/// 竖排标签中的一项。
pub(crate) struct Tab {
    pub(crate) id: TabId,
    pub(crate) content: TabContent,
    /// 用户重命名后的标题，优先于程序设置的标题。
    pub(crate) custom_title: Option<SharedString>,
}

pub(crate) enum TabContent {
    Terminals(Terminals),
    /// 设置页，全窗口唯一。
    Settings(Entity<SettingsView>),
    Sftp(SftpTab),
}

/// SFTP 双栏标签。连接提示显示在标签内。
pub(crate) struct SftpTab {
    pub(crate) view: Entity<SftpView>,
    /// 主机名。
    pub(crate) title: SharedString,
    pub(crate) prompt: Option<PanePrompt>,
}

impl Tab {
    pub(crate) fn terminals(&self) -> Option<&Terminals> {
        match &self.content {
            TabContent::Terminals(t) => Some(t),
            TabContent::Settings(_) | TabContent::Sftp(..) => None,
        }
    }

    pub(crate) fn terminals_mut(&mut self) -> Option<&mut Terminals> {
        match &mut self.content {
            TabContent::Terminals(t) => Some(t),
            TabContent::Settings(_) | TabContent::Sftp(..) => None,
        }
    }

    pub(crate) fn is_settings(&self) -> bool {
        matches!(self.content, TabContent::Settings(_))
    }

    /// SSH 标签用主机的图标。
    pub(crate) fn icon(&self, cx: &gpui_kit::App) -> Icon {
        match &self.content {
            TabContent::Terminals(t) => match t.focused_pane().map(|p| &p.launch) {
                Some(Launch::Ssh { host, .. }) => crate::config::ConfigState::hosts(cx)
                    .get(host)
                    .map(crate::tabs::host_icon)
                    .unwrap_or_else(|| Icon::new(gpui_kit::assets::IconName::Server)),
                _ => crate::assets::prompt_icon(),
            },
            TabContent::Settings(_) => Icon::new(IconName::Settings),
            TabContent::Sftp(..) => Icon::new(gpui_kit::assets::IconName::Folders),
        }
    }

    pub(crate) fn new(id: TabId, content: TabContent) -> Self {
        Self {
            id,
            content,
            custom_title: None,
        }
    }

    /// 用户命名优先；终端标签其次显示当前窗格的标题，程序未设置时用默认名称。
    pub(crate) fn display_title(&self) -> SharedString {
        if let Some(title) = &self.custom_title {
            return title.clone();
        }
        match &self.content {
            TabContent::Terminals(t) => match t.focused_pane() {
                Some(p) => match (p.title.as_deref(), &p.launch) {
                    (Some(title), _) if !title.is_empty() => title.to_owned().into(),
                    (_, Launch::Ssh { name, .. }) => name.clone(),
                    _ => "Shell".into(),
                },
                None => "Shell".into(),
            },
            TabContent::Settings(_) => t!("settings.title").into(),
            TabContent::Sftp(sftp) => sftp.title.clone(),
        }
    }
}

/// 终端标签的内容：分屏树与其中的窗格。至少含一个窗格；最后一个窗格关闭时标签随之关闭。
pub(crate) struct Terminals {
    pub(crate) tree: PaneTree,
    pub(crate) panes: Vec<Pane>,
    pub(crate) focused: PaneId,
    /// 最大化显示的窗格。
    pub(crate) zoomed: Option<PaneId>,
}

impl Terminals {
    pub(crate) fn new(pane: Pane) -> Self {
        Self {
            tree: PaneTree::Leaf(pane.id),
            focused: pane.id,
            panes: vec![pane],
            zoomed: None,
        }
    }

    pub(crate) fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub(crate) fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    pub(crate) fn focused_pane(&self) -> Option<&Pane> {
        self.pane(self.focused).or(self.panes.first())
    }

    pub(crate) fn pane_of_session(&self, session: SessionId) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|p| p.session.id() == session)
            .map(|p| p.id)
    }
}

/// 把终端视图的输入接到会话句柄上。
pub(crate) struct SessionIo(pub(crate) SessionHandle);

impl TerminalIo for SessionIo {
    fn write(&self, bytes: Vec<u8>) {
        self.0.write(bytes);
    }

    fn resize(&self, size: TermSize) {
        self.0.resize(size);
    }
}
