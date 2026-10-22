//! 本机可用的 shell 配置档探测。

use std::path::{Path, PathBuf};

use crate::LocalShell;

/// 一个可供“新建标签”选择的本地 shell。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    /// 显示名，如 `zsh`、`PowerShell 7`、`Ubuntu (WSL)`。
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl ShellProfile {
    pub fn to_shell(&self) -> LocalShell {
        LocalShell {
            program: Some(self.program.clone()),
            args: self.args.clone(),
            ..LocalShell::default()
        }
    }
}

/// 探测本机 shell，默认 shell 排在最前。会读文件、在 Windows 上还会运行 `wsl.exe`，不要在 UI 线程调用。
pub fn detect_shells() -> Vec<ShellProfile> {
    platform::detect()
}

/// 设置里没指定程序时启动的 shell；`None` 表示交给 portable-pty 的默认值（Unix 上为登录 shell）。
pub(crate) fn default_shell() -> Option<ShellProfile> {
    platform::default_shell()
}

#[cfg(unix)]
mod platform {
    use super::*;

    /// 不适合交互使用的登录 shell。
    const SKIP: &[&str] = &[
        "rbash",
        "git-shell",
        "nologin",
        "false",
        "systemd-home-fallback-shell",
    ];

    pub(super) fn detect() -> Vec<ShellProfile> {
        let listed = std::fs::read_to_string("/etc/shells").unwrap_or_default();
        let default = std::env::var_os("SHELL").map(PathBuf::from);
        from_list(&listed, default.as_deref(), |p| p.canonicalize().ok())
    }

    pub(super) fn default_shell() -> Option<ShellProfile> {
        None
    }

    /// `resolve` 返回去重用的真实路径，不存在的文件返回 `None`。
    pub(super) fn from_list(
        listed: &str,
        default: Option<&Path>,
        resolve: impl Fn(&Path) -> Option<PathBuf>,
    ) -> Vec<ShellProfile> {
        let candidates = default.into_iter().map(Path::to_path_buf).chain(
            listed
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with('/'))
                .map(PathBuf::from),
        );
        let mut seen = Vec::new();
        let mut profiles = Vec::new();
        for path in candidates {
            // /bin 常是 /usr/bin 的链接，sh 常指向 bash：按真实路径去重，并以真实程序名显示和启动
            let Some(real) = resolve(&path) else {
                continue;
            };
            let Some(name) = real.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
                continue;
            };
            if SKIP.contains(&name.as_str()) || seen.contains(&real) {
                continue;
            }
            seen.push(real.clone());
            profiles.push(ShellProfile {
                name,
                program: real,
                args: Vec::new(),
            });
        }
        profiles
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub(super) fn detect() -> Vec<ShellProfile> {
        let mut profiles = powershells();
        let system = system_dir();
        profiles.push(profile("cmd", system.join("cmd.exe"), &[]));

        let git_bash = program_files().join(r"Git\bin\bash.exe");
        if git_bash.exists() {
            profiles.push(profile("Git Bash", git_bash, &["--login", "-i"]));
        }
        let wsl = system.join("wsl.exe");
        if wsl.exists() {
            for distro in wsl_distros(&wsl) {
                let name = format!("{distro} (WSL)");
                profiles.push(profile(&name, wsl.clone(), &["-d", &distro]));
            }
        }
        profiles
    }

    /// 与 Windows Terminal、VS Code 一致，默认用 PowerShell 而不是 `%ComSpec%`（cmd）。
    pub(super) fn default_shell() -> Option<ShellProfile> {
        powershells().into_iter().next()
    }

    /// PowerShell 7 在前；Windows PowerShell 是 Win10/11 自带的。
    fn powershells() -> Vec<ShellProfile> {
        let mut profiles = Vec::new();
        let pwsh = find_in_path("pwsh.exe").or_else(|| {
            Some(program_files().join(r"PowerShell\7\pwsh.exe")).filter(|p| p.exists())
        });
        if let Some(pwsh) = pwsh {
            profiles.push(profile("PowerShell 7", pwsh, &["-NoLogo"]));
        }
        let powershell = system_dir().join(r"WindowsPowerShell\v1.0\powershell.exe");
        if powershell.exists() {
            profiles.push(profile("Windows PowerShell", powershell, &["-NoLogo"]));
        }
        profiles
    }

    fn program_files() -> PathBuf {
        std::env::var_os("ProgramFiles")
            .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from)
    }

    fn system_dir() -> PathBuf {
        std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32")
    }

    fn profile(name: &str, program: PathBuf, args: &[&str]) -> ShellProfile {
        ShellProfile {
            name: name.to_owned(),
            program,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    fn find_in_path(exe: &str) -> Option<PathBuf> {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(exe))
            .find(|p| p.exists())
    }

    /// `wsl.exe -l -q` 输出 UTF-16LE，每行一个发行版。
    fn wsl_distros(wsl: &Path) -> Vec<String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Ok(out) = std::process::Command::new(wsl)
            .args(["-l", "-q"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            return Vec::new();
        };
        let units: Vec<u16> = out
            .stdout
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        String::from_utf16_lossy(&units)
            .lines()
            .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn dedupes_links_and_skips_restricted() {
        let listed = "# comment\n/bin/sh\n/bin/bash\n/usr/bin/bash\n/usr/bin/rbash\n/usr/bin/zsh\n/usr/bin/fish\n";
        // 模拟 Arch：/bin → /usr/bin，sh → bash；fish 未安装
        let resolve = |p: &Path| {
            let s = p
                .to_str()?
                .replace("/bin/", "/usr/bin/")
                .replace("/usr/usr/", "/usr/");
            let s = if s.ends_with("/sh") {
                s.replace("/sh", "/bash")
            } else {
                s
            };
            (!s.ends_with("fish")).then(|| PathBuf::from(s))
        };
        let names: Vec<_> = platform::from_list(listed, Some(Path::new("/usr/bin/zsh")), resolve)
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["zsh", "bash"]);
    }
}
