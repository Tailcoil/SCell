//! `cargo xtask dist`：构建 release 并打包到 `target/dist`。

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::{licenses, run};

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Format {
    /// Linux 为 tar.gz，Windows 为 zip。
    Archive,
    /// 需要 PATH 中有 appimagetool。
    Appimage,
    /// 需要 dpkg-deb。
    Deb,
    /// 需要 rpmbuild（Debian 系装 `rpm` 包即可）。
    Rpm,
    /// Velopack 的安装程序、便携包与更新源。只能在 Windows 上构建，需要与 Cargo.lock 中 velopack
    /// 同版本的 vpk（`dotnet tool install --global vpk --version <版本>`）。
    Velopack,
    /// 装有 SCell.app 的磁盘映像。只能在 macOS 上构建，未经公证，只做临时签名。
    Dmg,
}

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SUMMARY: &str = "More than fast: a clean, intuitive, all-in-one modern terminal";
const HOMEPAGE: &str = env!("CARGO_PKG_REPOSITORY");
/// Debian 要求“名字 <邮箱>”格式。
const MAINTAINER: &str = "Tailcoil <110141765+Tailcoil@users.noreply.github.com>";
/// 安装目录为 %LocalAppData%\Tailcoil.SCell。不能叫 SCell：Windows 路径不分大小写，会与程序自己的
/// %LocalAppData%\scell（状态与日志）重合，安装时被当成旧版本移走，卸载时被一并删除。
const VELOPACK_ID: &str = "Tailcoil.SCell";
const BUNDLE_ID: &str = "io.github.tailcoil.scell";
/// 与 CI 的 MACOSX_DEPLOYMENT_TARGET 一致；DMG 的 ULMO 压缩格式也要求 10.15 起。
const MACOS_MIN: &str = "10.15";

/// Debian 包的运行时依赖：直接链接的库，以及 GPUI 运行时动态加载的 Wayland 与图形库。
const DEB_DEPENDS: &str = "libc6, libgcc-s1, libxcb1, libxkbcommon0, libxkbcommon-x11-0, \
    libwayland-client0, libwayland-egl1, libvulkan1 | libegl1";

/// RPM 只需声明运行时动态加载的库，直接链接的由 rpmbuild 从 ELF 自动生成。
/// 按 soname 声明，不同发行版的包名都能满足；`(64bit)` 后缀只适用于 64 位架构。
const RPM_REQUIRES: &[&str] = &[
    "libwayland-client.so.0()(64bit)",
    "libwayland-egl.so.1()(64bit)",
    "(libvulkan.so.1()(64bit) or libEGL.so.1()(64bit))",
];

pub(crate) fn workspace_root() -> anyhow::Result<&'static Path> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask 应位于工作区根目录下")
}

/// 构建 release，返回程序路径。
fn build_release(root: &Path) -> anyhow::Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    run(Command::new(cargo).current_dir(root).args([
        "build",
        "--release",
        "--locked",
        "-p",
        "scell",
    ]))?;
    let exe = format!("scell{}", std::env::consts::EXE_SUFFIX);
    Ok(root.join("target/release").join(exe))
}

pub(crate) fn dist(formats: &[Format]) -> anyhow::Result<()> {
    let root = workspace_root()?;
    let bin = build_release(root)?;
    let exe = format!("scell{}", std::env::consts::EXE_SUFFIX);
    let out = root.join("target/dist");
    std::fs::create_dir_all(&out)?;
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    let name = format!("scell-{VERSION}-{arch}-{os}");
    // 各种包都附带自身与依赖的许可证
    let notices = licenses::notices(root, &out)?;
    let pkg = Package {
        root,
        bin: &bin,
        notices: &notices,
    };
    for format in formats {
        let path = match format {
            Format::Archive => archive(&pkg, &exe, &out, &name)?,
            Format::Appimage => appimage(&pkg, &out, &name)?,
            Format::Deb => deb(&pkg, &out, arch)?,
            Format::Rpm => rpm(&pkg, &out, arch)?,
            Format::Velopack => velopack(&pkg, &out, &name)?,
            Format::Dmg => dmg(&pkg, &out, &name)?,
        };
        println!("{}", path.display());
    }
    Ok(())
}

/// 打进包里的文件。
struct Package<'a> {
    root: &'a Path,
    bin: &'a Path,
    /// 第三方许可证全文。
    notices: &'a Path,
}

impl Package<'_> {
    /// 本项目与第三方的许可证放进 `dir`。
    fn licenses(&self, dir: &Path) -> anyhow::Result<()> {
        copy(&self.root.join("LICENSE"), &dir.join("LICENSE"))?;
        copy(self.notices, &dir.join("THIRD-PARTY-LICENSES.html"))
    }
}

/// 程序、桌面文件、图标与许可证按 FHS 放进 `prefix`。
fn install_linux(pkg: &Package, prefix: &Path) -> anyhow::Result<()> {
    let root = pkg.root;
    copy(pkg.bin, &prefix.join("bin/scell"))?;
    pkg.licenses(&prefix.join("share/doc/scell"))?;
    copy(
        &root.join("packaging/linux/scell.desktop"),
        &prefix.join("share/applications/scell.desktop"),
    )?;
    copy(
        &root.join("assets/icons/scell.svg"),
        &prefix.join("share/icons/hicolor/scalable/apps/scell.svg"),
    )
}

/// `packaging/windows/splash.png` 由图标生成：
/// `magick -respect-parentheses -size 480x300 radial-gradient:'#173f48-#06141a' \( -background none -density 192
/// assets/icons/scell.svg -resize 132x132 \) -gravity north -geometry +0+52 -composite -density 72
/// -font IBM-Plex-Sans-Light -fill '#f4ecd2' -pointsize 30 -annotate +0+200 SCell -depth 8 -strip splash.png`
fn velopack(pkg: &Package, out: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let root = pkg.root;
    if !cfg!(windows) {
        bail!("Velopack 安装包只能在 Windows 上构建");
    }
    let app = fresh_dir(&out.join("velopack-app"))?;
    copy(pkg.bin, &app.join("scell.exe"))?;
    pkg.licenses(&app)?;
    let releases = fresh_dir(&out.join("velopack"))?;
    run(Command::new("vpk")
        .arg("pack")
        .args(["--packId", VELOPACK_ID, "--packVersion", VERSION])
        .args(["--packTitle", "SCell", "--packAuthors", "Tailcoil"])
        .args(["--mainExe", "scell.exe", "--shortcuts", "StartMenuRoot"])
        .args(["--splashProgressColor", "#F6C862"])
        .arg("--packDir")
        .arg(&app)
        .arg("--icon")
        .arg(root.join("assets/icons/scell.ico"))
        .arg("--splashImage")
        .arg(root.join("packaging/windows/splash.png"))
        .arg("--outputDir")
        .arg(&releases))
    .context(
        "需要 vpk：dotnet tool install --global vpk --version <Cargo.lock 中 velopack 的版本>",
    )?;
    // 更新源（releases.win.json 与 nupkg）保持原名，程序按这些名字下载；安装程序与便携包按本项目的规则改名。
    // 另外生成的 RELEASES 只给旧版 Squirrel 用，assets.win.json 只给 vpk upload 用，不发布
    let setup = out.join(format!("{name}-setup.exe"));
    for entry in std::fs::read_dir(&releases)? {
        let path = entry?.path();
        let file = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or_default();
        let dest = if file.ends_with("-Setup.exe") {
            setup.clone()
        } else if file.ends_with("-Portable.zip") {
            out.join(format!("{name}-portable.zip"))
        } else if file.ends_with(".nupkg") || file.starts_with("releases.") {
            out.join(file)
        } else {
            continue;
        };
        copy(&path, &dest)?;
    }
    Ok(setup)
}

fn dmg(pkg: &Package, out: &Path, name: &str) -> anyhow::Result<PathBuf> {
    if !cfg!(target_os = "macos") {
        bail!("DMG 只能在 macOS 上构建");
    }
    let stage = fresh_dir(&out.join("dmg"))?;
    let app = stage.join("SCell.app");
    let contents = app.join("Contents");
    copy(pkg.bin, &contents.join("MacOS/scell"))?;
    copy(
        &pkg.root.join("packaging/macos/scell.icns"),
        &contents.join("Resources/scell.icns"),
    )?;
    pkg.licenses(&contents.join("Resources"))?;
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>SCell</string>
    <key>CFBundleDisplayName</key><string>SCell</string>
    <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
    <key>CFBundleExecutable</key><string>scell</string>
    <key>CFBundleIconFile</key><string>scell</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>{VERSION}</string>
    <key>CFBundleVersion</key><string>{VERSION}</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>LSMinimumSystemVersion</key><string>{MACOS_MIN}</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
"#
    );
    std::fs::write(contents.join("Info.plist"), plist)?;
    // 没有开发者证书，只做临时签名：Apple Silicon 拒绝运行未签名的程序。用户首次打开需在系统设置里放行
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&app))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))?;
    let file = out.join(format!("{name}.dmg"));
    run(Command::new("hdiutil")
        .args(["create", "-ov", "-volname", "SCell", "-format", "ULMO"])
        .arg("-srcfolder")
        .arg(&stage)
        .arg(&file))?;
    Ok(file)
}

fn archive(pkg: &Package, exe: &str, out: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let root = pkg.root;
    let stage = fresh_dir(&out.join(name))?;
    copy(pkg.bin, &stage.join(exe))?;
    pkg.licenses(&stage)?;
    if cfg!(target_os = "linux") {
        copy(
            &root.join("packaging/linux/scell.desktop"),
            &stage.join("scell.desktop"),
        )?;
        copy(
            &root.join("assets/icons/scell.svg"),
            &stage.join("scell.svg"),
        )?;
    }
    // Windows 10 起自带 bsdtar，`-a` 按扩展名选择 zip
    let file = out.join(if cfg!(windows) {
        format!("{name}.zip")
    } else {
        format!("{name}.tar.gz")
    });
    run(Command::new("tar")
        .arg(if cfg!(windows) { "-a" } else { "-z" })
        .arg("-cf")
        .arg(&file)
        .arg("-C")
        .arg(out)
        .arg(name))?;
    Ok(file)
}

fn appimage(pkg: &Package, out: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let root = pkg.root;
    if !cfg!(target_os = "linux") {
        bail!("AppImage 只能在 Linux 上构建");
    }
    let app = fresh_dir(&out.join("SCell.AppDir"))?;
    install_linux(pkg, &app.join("usr"))?;
    copy(
        &root.join("packaging/linux/scell.desktop"),
        &app.join("scell.desktop"),
    )?;
    copy(&root.join("assets/icons/scell.svg"), &app.join("scell.svg"))?;
    let app_run = app.join("AppRun");
    std::fs::write(
        &app_run,
        "#!/bin/sh\nexec \"$(dirname \"$(readlink -f \"$0\")\")/usr/bin/scell\" \"$@\"\n",
    )?;
    make_executable(&app_run)?;
    let file = out.join(format!("{name}.AppImage"));
    run(Command::new("appimagetool")
        .env("ARCH", std::env::consts::ARCH)
        .arg(&app)
        .arg(&file))
    .context("需要 PATH 中有 appimagetool")?;
    Ok(file)
}

fn deb(pkg: &Package, out: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let arch = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let stage = fresh_dir(&out.join("deb"))?;
    install_linux(pkg, &stage.join("usr"))?;
    let size_kib = std::fs::metadata(pkg.bin)?.len() / 1024;
    let control = format!(
        "Package: scell\nVersion: {VERSION}\nArchitecture: {arch}\nMaintainer: {MAINTAINER}\n\
         Installed-Size: {size_kib}\nDepends: {DEB_DEPENDS}\nSection: x11\nPriority: optional\n\
         Homepage: {HOMEPAGE}\nDescription: {SUMMARY}\n"
    );
    std::fs::create_dir_all(stage.join("DEBIAN"))?;
    std::fs::write(stage.join("DEBIAN/control"), control)?;
    let file = out.join(format!("scell_{VERSION}_{arch}.deb"));
    run(Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&stage)
        .arg(&file))
    .context("需要 dpkg-deb")?;
    Ok(file)
}

fn rpm(pkg: &Package, out: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let top = fresh_dir(&out.join("rpmbuild"))?;
    let stage = top.join("stage");
    install_linux(pkg, &stage.join("usr"))?;
    let requires: String = RPM_REQUIRES
        .iter()
        .map(|r| format!("Requires: {r}\n"))
        .collect();
    // 与 deb 一样保留符号表，所以关掉 strip 等安装后处理，也不生成 debuginfo 子包
    let spec = format!(
        "Name: scell\nVersion: {VERSION}\nRelease: 1\nSummary: {SUMMARY}\nLicense: Apache-2.0\n\
         URL: {HOMEPAGE}\n{requires}\n\
         %global debug_package %{{nil}}\n%global __os_install_post %{{nil}}\n\
         %global _build_id_links none\n\n\
         %description\n{SUMMARY}.\n\n\
         %install\ncp -a '{stage}/.' %{{buildroot}}/\n\n\
         %files\n%{{_bindir}}/scell\n%{{_datadir}}/applications/scell.desktop\n\
         %{{_datadir}}/icons/hicolor/scalable/apps/scell.svg\n%{{_docdir}}/scell\n",
        stage = stage.display(),
    );
    let spec_file = top.join("scell.spec");
    std::fs::write(&spec_file, spec)?;
    run(Command::new("rpmbuild")
        .arg("-bb")
        .arg("--define")
        .arg(format!("_topdir {}", top.display()))
        .arg("--define")
        .arg(format!("_rpmdir {}", out.display()))
        .arg("--define")
        .arg("_build_name_fmt %%{NAME}-%%{VERSION}-%%{RELEASE}.%%{ARCH}.rpm")
        .arg(&spec_file))
    .context("需要 rpmbuild")?;
    Ok(out.join(format!("scell-{VERSION}-1.{arch}.rpm")))
}

fn fresh_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    Ok(dir.to_owned())
}

fn copy(from: &Path, to: &Path) -> anyhow::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to).with_context(|| format!("无法复制 {}", from.display()))?;
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> anyhow::Result<()> {
    Ok(())
}
