//! 开发辅助任务：`cargo xtask <命令>`。

mod dist;
mod licenses;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 以当前用户身份启动或停止本地测试用 sshd（仅监听 127.0.0.1，不修改系统配置），
    /// 以及一个只装有测试私钥的 ssh-agent。
    Sshd {
        #[command(subcommand)]
        action: SshdAction,
    },
    /// 启动本地测试 sshd 与 agent，运行 SSH 与会话层的集成测试，结束后停止。
    It {
        #[arg(long, default_value_t = 2299)]
        port: u16,
    },
    /// 重新生成程序内嵌的第三方依赖清单；`--check` 只检查是否过期。
    Licenses {
        #[arg(long)]
        check: bool,
    },
    /// 构建 release 并打包到 target/dist。
    Dist {
        #[arg(long, value_enum, value_delimiter = ',', default_value = "archive")]
        format: Vec<dist::Format>,
    },
}

#[derive(Subcommand)]
enum SshdAction {
    Up {
        #[arg(long, default_value_t = 2222)]
        port: u16,
    },
    Down,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Sshd {
            action: SshdAction::Up { port },
        } => {
            for (key, value) in sshd_up(port)? {
                println!("{key}={value}");
            }
            Ok(())
        }
        Cmd::Sshd {
            action: SshdAction::Down,
        } => sshd_down(),
        Cmd::It { port } => integration(port),
        Cmd::Licenses { check } => licenses::licenses(dist::workspace_root()?, check),
        Cmd::Dist { format } => dist::dist(&format),
    }
}

fn integration(port: u16) -> anyhow::Result<()> {
    let vars = sshd_up(port)?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    // 会话层测试会读写同一个本地 sshd，串行运行避免互相干扰
    let result = run(Command::new(cargo)
        .args(["test", "-p", "scell-ssh", "-p", "scell-session", "--"])
        .args(["--ignored", "--test-threads", "1"])
        .envs(vars));
    sshd_down()?;
    result
}

fn state_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/xtask-sshd")
}

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let status = cmd.status().with_context(|| format!("无法执行 {cmd:?}"))?;
    if !status.success() {
        bail!("{cmd:?} 退出码 {status}");
    }
    Ok(())
}

fn keygen(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        run(Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", ""])
            .arg("-f")
            .arg(path))?;
    }
    Ok(())
}

fn write_exec(path: &Path, content: &str) -> anyhow::Result<()> {
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// 返回测试所需的环境变量。
fn sshd_up(port: u16) -> anyhow::Result<Vec<(&'static str, String)>> {
    let dir = state_dir();
    let samples = dir.join("files");
    std::fs::create_dir_all(&samples)?;
    let dir = dir.canonicalize()?;
    keygen(&dir.join("host_ed25519"))?;
    keygen(&dir.join("client_ed25519"))?;
    std::fs::copy(dir.join("client_ed25519.pub"), dir.join("authorized_keys"))?;

    // 非 UTF-8 文件名样本：GBK 编码的“中文.txt”
    std::fs::write(samples.join("utf8-中文.txt"), "y")?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let gbk = std::ffi::OsStr::from_bytes(b"\xd6\xd0\xce\xc4.txt");
        std::fs::write(samples.join(gbk), "x")?;
    }

    let d = dir.display();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    write_exec(
        &bin.join("su"),
        r#"#!/bin/sh
[ "$1" = "-" ] && shift
user="$1"
shift
printf "Password: "
stty -echo 2>/dev/null
IFS= read -r pw
stty echo 2>/dev/null
printf "\n"
if [ "$pw" != "scell-su-pw" ]; then
    printf "su: Authentication failure\n" >&2
    exit 1
fi
export SCELL_SWITCHED_USER="$user"
if [ "$1" = "-c" ]; then
    exec /bin/sh -c "$2"
fi
ENV="" PS1="switched-${user}# " exec /bin/sh -i
"#,
    )?;
    write_exec(
        &bin.join("sudo"),
        r#"#!/bin/sh
prompt="Password: "
user="root"
shell_mode=0
while [ $# -gt 0 ]; do
    case "$1" in
        -p) prompt="$2"; shift 2 ;;
        -u) user="$2"; shift 2 ;;
        -s|-i) shell_mode=1; shift ;;
        *) break ;;
    esac
done
printf "%s" "$prompt"
stty -echo 2>/dev/null
IFS= read -r pw
stty echo 2>/dev/null
printf "\n"
if [ "$pw" != "scell-sudo-pw" ]; then
    printf "Sorry, try again.\nsudo: 1 incorrect password attempt\n" >&2
    exit 1
fi
export SCELL_SWITCHED_USER="$user"
if [ "$shell_mode" = "1" ]; then
    ENV="" PS1="switched-${user}# " exec /bin/sh -i
fi
exec "$@"
"#,
    )?;

    // 假的 su、sudo 经 SetEnv 放到 PATH 最前面。不用 ForceCommand 换 shell：会话保持用户自己的登录 shell，
    // 其他测试的环境不变；登录 shell 可能被 /etc/profile 重设 PATH，用到时由测试自己 export
    let config = format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_ed25519\n\
         AuthorizedKeysFile {d}/authorized_keys\nPidFile {d}/sshd.pid\nUsePAM no\n\
         StrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
         SetEnv PATH={}:/usr/local/bin:/usr/bin:/bin\n\
         Subsystem sftp internal-sftp\n",
        bin.display()
    );
    std::fs::write(dir.join("sshd_config"), config)?;
    run(Command::new(sshd_binary()?)
        .arg("-f")
        .arg(dir.join("sshd_config"))
        .arg("-E")
        .arg(dir.join("sshd.log")))?;
    let agent = agent_up(&dir)?;

    let user = std::env::var("USER").unwrap_or_default();
    let target = format!(
        "127.0.0.1:{port}:{user}:{}:{}",
        dir.join("client_ed25519").display(),
        samples.canonicalize()?.display()
    );
    Ok(vec![
        ("SCELL_SSH_IT", target),
        ("SCELL_SSH_IT_AGENT", agent.display().to_string()),
    ])
}

/// 启动独立的 ssh-agent 并载入测试私钥，返回其套接字。不影响用户自己的 agent。
fn agent_up(dir: &Path) -> anyhow::Result<PathBuf> {
    let socket = dir.join("agent.sock");
    let _ = std::fs::remove_file(&socket);
    let out = Command::new("ssh-agent")
        .arg("-s")
        .arg("-a")
        .arg(&socket)
        .output()
        .context("无法执行 ssh-agent")?;
    // 输出形如 `SSH_AGENT_PID=1234; export SSH_AGENT_PID;`
    let text = String::from_utf8_lossy(&out.stdout);
    let pid = text
        .split("SSH_AGENT_PID=")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .context("无法解析 ssh-agent 的 PID")?;
    std::fs::write(dir.join("agent.pid"), pid)?;
    run(Command::new("ssh-add")
        .arg("-q")
        .arg(dir.join("client_ed25519"))
        .env("SSH_AUTH_SOCK", &socket))?;
    Ok(socket)
}

/// sshd 必须以绝对路径启动；各发行版位置不同（Arch 为 /usr/bin，Debian 系为 /usr/sbin）。
fn sshd_binary() -> anyhow::Result<PathBuf> {
    ["/usr/sbin/sshd", "/usr/bin/sshd"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .context("未找到 sshd，请先安装 OpenSSH 服务端")
}

fn sshd_down() -> anyhow::Result<()> {
    let agent = state_dir().join("agent.pid");
    if let Ok(pid) = std::fs::read_to_string(&agent) {
        let _ = Command::new("kill").arg(pid.trim()).status();
        let _ = std::fs::remove_file(agent);
    }
    let pid_file = state_dir().join("sshd.pid");
    let pid = std::fs::read_to_string(&pid_file).context("sshd 未在运行")?;
    run(Command::new("kill").arg(pid.trim()))?;
    let _ = std::fs::remove_file(pid_file);
    Ok(())
}
