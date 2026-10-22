//! 应用资源：组件库默认图标，本应用额外用到的 Lucide 图标（新用到的在此登记），以及自绘的图标。

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use gpui_kit::component::Icon;
use gpui_kit::{AssetSource, Image, ImageFormat, Result, SharedString};

/// 带独角的提示符，按 Lucide 的网格与笔画绘制，用作本地终端的图标。
const PROMPT_ICON: (&str, &[u8]) = (
    "icons/scell-prompt.svg",
    include_bytes!("../assets/icons/scell-prompt.svg"),
);

pub(crate) fn prompt_icon() -> Icon {
    Icon::empty().path(PROMPT_ICON.0)
}

/// 彩色的应用图标，与安装包里的是同一个文件。
pub(crate) fn app_logo() -> Arc<Image> {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    LOGO.get_or_init(|| {
        let bytes = include_bytes!("../../../assets/icons/scell.svg");
        Arc::new(Image::from_bytes(ImageFormat::Svg, bytes.to_vec()))
    })
    .clone()
}

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        AArrowDown,
        AArrowUp,
        AppWindow,
        ArrowDownUp,
        Bot,
        CaseSensitive,
        ChevronDown,
        ChevronUp,
        Cloud,
        Columns2,
        Container,
        Cpu,
        Database,
        Download,
        FileSymlink,
        FlaskConical,
        FolderPlus,
        FolderUp,
        Folders,
        Globe,
        HardDrive,
        House,
        Import,
        Keyboard,
        KeyRound,
        Laptop,
        ListOrdered,
        Layers,
        LockKeyhole,
        Monitor,
        Pencil,
        PictureInPicture2,
        Regex,
        Rocket,
        RotateCcw,
        Router,
        Rows2,
        Server,
        Shield,
        Sparkles,
        SquareX,
        Trash,
        Upload,
        UserKey
    ]
);

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == PROMPT_ICON.0 {
            return Ok(Some(Cow::Borrowed(PROMPT_ICON.1)));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        if PROMPT_ICON.0.starts_with(path) {
            paths.push(PROMPT_ICON.0.into());
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
