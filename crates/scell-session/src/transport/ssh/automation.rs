//! 自动操作：在交互 shell 中按步骤发送、等待输出、用 su/sudo 切换用户，以及提权打开 SFTP。
//!
//! 匹配只看步骤开始之后的新输出：发送一行后跳过它的回显；切换用户前先让 shell 打印一个
//! 只在输出里完整出现的标记，标记之前晚到的旧输出一律忽略。

use std::sync::Arc;
use std::time::Duration;

use rust_i18n::t;
use scell_config::hosts::{HostProfile, Step, SwitchMethod};
use scell_ssh::{ShellChannel, ShellEvent};
use tokio::time::Instant;

use super::auth::Hop;
use super::{Interaction, SshSession, resolved_user};
use crate::Encoding;
use crate::codec::Encoder;
use crate::pool::Link;
use crate::pump::Pump;

const STEP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SWITCH_TRIES: usize = 3;
/// 匹配缓冲只保留最近的这么多字节，持续大量输出时不会无限增长。
const BUF_LIMIT: usize = 8 * 1024;
/// 末行像提示符后还要静默这么久才算数，避免把分包到达的 MOTD（如 `Swap usage: 0%`）当成提示符。
const PROMPT_SETTLE: Duration = Duration::from_millis(100);
/// 登录后没识别出提示符时，首屏输出静默这么久即视为就绪。
const OUTPUT_SETTLE: Duration = Duration::from_millis(200);
const SFTP_READY_MARKER: &[u8] = b"@@scell-sftp";
/// 切换用户前由 shell 打印的标记；命令行里写作 `%s` 拼接，回显不含完整标记。
const SWITCH_MARKER: &[u8] = b"@@scell-switch";

/// 自动操作单步的执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StepOutcome {
    Completed,
    Aborted,
    ChannelClosed,
}

/// 自动操作期间的 shell 读写：输出照常交给终端，同时留一份供匹配。
pub(super) struct ShellIo<'a> {
    pub(super) shell: &'a mut ShellChannel,
    pub(super) pump: &'a mut Pump,
    pub(super) encoder: &'a Encoder,
    pub(super) code: &'a mut Option<u32>,
    encoding: Encoding,
    /// 尚未被步骤消费的输出。
    buf: Vec<u8>,
    /// 接下来的输出以刚发送那行的回显开头，第一个换行之前的内容不参与匹配。
    /// 远端关闭了回显时会多跳过一行输出。
    skip_echo: bool,
}

impl<'a> ShellIo<'a> {
    pub(super) fn new(
        shell: &'a mut ShellChannel,
        pump: &'a mut Pump,
        encoder: &'a Encoder,
        code: &'a mut Option<u32>,
        encoding: Encoding,
    ) -> Self {
        Self {
            shell,
            pump,
            encoder,
            code,
            encoding,
            buf: Vec::new(),
            skip_echo: false,
        }
    }

    fn feed(&mut self, data: &[u8]) {
        self.pump.on_output(data);
        let mut data = data;
        if self.skip_echo {
            let Some(pos) = data.iter().position(|&b| b == b'\n') else {
                return;
            };
            data = &data[pos + 1..];
            self.skip_echo = false;
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() > BUF_LIMIT {
            self.buf.drain(..self.buf.len() - BUF_LIMIT);
        }
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.skip_echo = false;
    }

    fn clean(&self) -> String {
        strip_ansi(&self.encoding.decode(&self.buf))
    }

    /// 发送一行（末尾回车），之后的匹配从这行的回显之后开始。
    async fn send_line(&mut self, text: &str) -> bool {
        self.buf.clear();
        self.skip_echo = true;
        let line = format!("{text}\r");
        self.shell
            .write(&self.encoder.encode(line.into_bytes()))
            .await
            .is_ok()
    }

    /// 消费到 `wait` 首次出现之后，保留同一数据包中后续到达的文本供下一步匹配。
    fn consume_through(&mut self, wait: &str) {
        let clean = self.clean();
        let Some(pos) = clean.find(wait) else {
            self.buf.clear();
            return;
        };
        let rest = &clean[pos + wait.len()..];
        self.buf = self
            .encoding
            .encode(rest)
            .map_or_else(|| rest.as_bytes().to_vec(), |b| b.into_owned());
    }
}

impl SshSession {
    /// 按顺序执行一组自动操作步骤，期间终端正常刷新与响应窗口尺寸变化。
    pub(super) async fn execute_steps(
        &mut self,
        link: &Arc<Link>,
        io: &mut ShellIo<'_>,
        steps: &[Step],
        on_login: bool,
    ) -> StepOutcome {
        io.reset();
        // 登录时若第一步直接发送或切换用户，先等初始 shell 提示符或首屏输出就绪
        if on_login
            && matches!(
                steps.first(),
                Some(Step::Send { .. } | Step::SwitchUser { .. })
            )
        {
            if self.wait_initial_prompt(io).await == StepOutcome::ChannelClosed {
                return StepOutcome::ChannelClosed;
            }
            io.reset();
        }

        for step in steps {
            match step {
                Step::Send { text } => {
                    if !io.send_line(text).await {
                        return StepOutcome::ChannelClosed;
                    }
                }
                Step::Expect { wait, send } => {
                    if !wait.is_empty() {
                        let outcome = self
                            .wait_output(io, |clean| clean.contains(wait.as_str()))
                            .await;
                        if outcome != StepOutcome::Completed {
                            return outcome;
                        }
                        io.consume_through(wait);
                    }
                    if !send.is_empty() && !io.send_line(send).await {
                        return StepOutcome::ChannelClosed;
                    }
                }
                Step::SwitchUser {
                    method,
                    user,
                    login,
                } => {
                    let outcome = self.switch_user(link, io, *method, user, *login).await;
                    if outcome != StepOutcome::Completed {
                        return outcome;
                    }
                }
            }
        }
        StepOutcome::Completed
    }

    /// 等待登录 shell 的初始输出就绪：末行像提示符且静默片刻即返回，否则在收到输出并静默一段时间后返回，最多等 3 秒。
    async fn wait_initial_prompt(&mut self, io: &mut ShellIo<'_>) -> StepOutcome {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut settle: Option<Instant> = None;
        loop {
            let sleep_until = settle.map_or(deadline, |s| s.min(deadline));
            tokio::select! {
                () = tokio::time::sleep_until(sleep_until) => return StepOutcome::Completed,
                event = io.shell.next_event() => match event {
                    ShellEvent::Data(data) => {
                        io.feed(&data);
                        let clean = io.clean();
                        if is_shell_prompt(&clean) {
                            settle = Some(Instant::now() + PROMPT_SETTLE);
                        } else if !clean.trim().is_empty() {
                            settle = Some(Instant::now() + OUTPUT_SETTLE);
                        }
                    }
                    ShellEvent::ExitStatus(c) => *io.code = Some(c),
                    ShellEvent::Closed => return StepOutcome::ChannelClosed,
                },
                Ok(bytes) = self.input.recv_async() => {
                    if io.shell.write(&io.encoder.encode(bytes)).await.is_err() {
                        return StepOutcome::ChannelClosed;
                    }
                }
                Some(s) = self.resize.recv() => {
                    self.size = s;
                    let _ = io.shell.resize(s.cols, s.rows).await;
                }
            }
        }
    }

    /// 等待终端输出满足 `predicate`，超时或用户按 Ctrl-C 则中止。
    async fn wait_output(
        &mut self,
        io: &mut ShellIo<'_>,
        mut predicate: impl FnMut(&str) -> bool,
    ) -> StepOutcome {
        if predicate(&io.clean()) {
            return StepOutcome::Completed;
        }
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => return StepOutcome::Aborted,
                event = io.shell.next_event() => match event {
                    ShellEvent::Data(data) => {
                        io.feed(&data);
                        if predicate(&io.clean()) {
                            return StepOutcome::Completed;
                        }
                    }
                    ShellEvent::ExitStatus(c) => *io.code = Some(c),
                    ShellEvent::Closed => return StepOutcome::ChannelClosed,
                },
                Ok(bytes) = self.input.recv_async() => {
                    let abort = bytes.contains(&0x03);
                    if io.shell.write(&io.encoder.encode(bytes)).await.is_err() {
                        return StepOutcome::ChannelClosed;
                    }
                    if abort {
                        return StepOutcome::Aborted;
                    }
                }
                Some(s) = self.resize.recv() => {
                    self.size = s;
                    let _ = io.shell.resize(s.cols, s.rows).await;
                }
            }
        }
    }

    /// 在交互式 shell 中执行 `su` 或 `sudo` 切换用户，自动处理密码提问、错误重试与凭据保存。
    /// 成功后缓冲里保留切换之后的输出（含新提示符），供后续 Expect 匹配。
    async fn switch_user(
        &mut self,
        link: &Arc<Link>,
        io: &mut ShellIo<'_>,
        method: SwitchMethod,
        user: &str,
        login: bool,
    ) -> StepOutcome {
        let _guard = link.switch_gate.lock().await;
        let hop = Hop::new(
            self.ui.clone(),
            self.target.host.id.clone(),
            link.secrets.clone(),
        );
        let login_user = resolved_user(&self.target.host);
        // 与 SSH 认证的提问一致只给主机名，界面按 `用户@主机` 显示
        let host_label = self.target.host.host.clone();
        // 不能用 LC_ALL=C 固定提示语言：sudo 默认保留 LC_*，非登录的 su 保留全部环境，
        // 都会带进切换后的 shell；su 的提示与报错按中英文两种匹配。
        // 行首空格让命令不进 shell 历史（HISTCONTROL=ignorespace）
        let target = shell_quote(user);
        let switch = match (method, login) {
            (SwitchMethod::Su, true) => format!("su - {target}"),
            (SwitchMethod::Su, false) => format!("su {target}"),
            (SwitchMethod::Sudo, true) => format!("sudo -p 'Password: ' -u {target} -i"),
            (SwitchMethod::Sudo, false) => format!("sudo -p 'Password: ' -u {target} -s"),
        };
        let cmd = format!(" printf '@@scell-%s\\n' switch; {switch}");

        for attempt in 0..MAX_SWITCH_TRIES {
            io.reset();
            if !io.send_line(&cmd).await {
                return StepOutcome::ChannelClosed;
            }
            // 由标记界定新输出；远端关闭回显时跳过回显行会把标记一并吞掉
            io.skip_echo = false;
            // 见到标记前的输出都属于之前的命令
            let mut started = false;
            let mut deadline = Instant::now() + STEP_TIMEOUT;
            // 末行像新提示符后静默到这一刻即判定结果
            let mut settle: Option<Instant> = None;
            loop {
                tokio::select! {
                    () = tokio::time::sleep_until(settle.map_or(deadline, |s| s.min(deadline))) => {
                        if settle.is_none_or(|s| s > deadline) {
                            return StepOutcome::Aborted;
                        }
                        if is_auth_failure(first_line(&io.clean())) {
                            if attempt + 1 < MAX_SWITCH_TRIES && hop.has_asked() {
                                break;
                            }
                            return StepOutcome::Aborted;
                        }
                        hop.commit(&self.shared).await;
                        return StepOutcome::Completed;
                    }
                    event = io.shell.next_event() => match event {
                        ShellEvent::Data(data) => {
                            io.feed(&data);
                            settle = None;
                            if !started {
                                let Some(end) = marker_line_end(&io.buf, SWITCH_MARKER) else {
                                    continue;
                                };
                                io.buf.drain(..end);
                                started = true;
                            }
                            let clean = io.clean();
                            if is_password_prompt(&clean) {
                                io.buf.clear();
                                let Some(pw) = hop
                                    .clone()
                                    .switch_password(method, user, &login_user, &host_label)
                                    .await
                                else {
                                    let _ = io.shell.write(b"\x03").await;
                                    return StepOutcome::Aborted;
                                };
                                let line = format!("{pw}\r");
                                if io
                                    .shell
                                    .write(&io.encoder.encode(line.into_bytes()))
                                    .await
                                    .is_err()
                                {
                                    return StepOutcome::ChannelClosed;
                                }
                                deadline = Instant::now() + STEP_TIMEOUT;
                            } else if is_shell_prompt(&clean) {
                                settle = Some(Instant::now() + PROMPT_SETTLE);
                            }
                        }
                        ShellEvent::ExitStatus(c) => *io.code = Some(c),
                        ShellEvent::Closed => return StepOutcome::ChannelClosed,
                    },
                    Ok(bytes) = self.input.recv_async() => {
                        let abort = bytes.contains(&0x03);
                        if io.shell.write(&io.encoder.encode(bytes)).await.is_err() {
                            return StepOutcome::ChannelClosed;
                        }
                        if abort {
                            return StepOutcome::Aborted;
                        }
                    }
                    Some(s) = self.resize.recv() => {
                        self.size = s;
                        let _ = io.shell.resize(s.cols, s.rows).await;
                    }
                }
            }
        }
        StepOutcome::Aborted
    }
}

/// 通过 PTY 执行 `su` 或 `sudo` 切换到目标用户后启动 `sftp-server`，把字节流交给 SFTP 客户端。
pub(super) async fn open_switched_sftp(
    link: Arc<Link>,
    host: &HostProfile,
    ui: &Arc<Interaction>,
    method: SwitchMethod,
    user: &str,
) -> Result<Arc<dyn scell_vfs::Vfs>, String> {
    let _guard = link.switch_gate.lock().await;
    let hop = Hop::new(ui.clone(), host.id.clone(), link.secrets.clone());
    let login_user = resolved_user(host);
    let host_label = host.host.clone();
    // 先把 PTY 设为 raw -echo 关闭回显与换行转换，再输出就绪标记并 exec sftp-server；
    // 标记拆成两段用 printf 拼接，命令回显本身不会包含完整标记。
    let script = r#"stty raw -echo 2>/dev/null; for p in /usr/lib/openssh/sftp-server /usr/libexec/openssh/sftp-server /usr/lib/ssh/sftp-server /usr/libexec/sftp-server /run/current-system/sw/libexec/sftp-server "$(command -v sftp-server 2>/dev/null)"; do if [ -n "$p" ] && [ -x "$p" ]; then printf "@@scell-%s\n" sftp; exec "$p"; fi; done; exit 127"#;
    let cmd = match method {
        SwitchMethod::Su => format!("env LC_ALL=C su {} -c '{}'", shell_quote(user), script),
        SwitchMethod::Sudo => format!(
            "env LC_ALL=C sudo -p 'Password: ' -u {} sh -c '{}'",
            shell_quote(user),
            script
        ),
    };

    for attempt in 0..MAX_SWITCH_TRIES {
        let mut ch = link
            .conn
            .open_exec_pty(&cmd)
            .await
            .map_err(|e| t!("sftp.open_failed", err = e).into_owned())?;
        let mut buf = Vec::new();
        let mut deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    return Err(t!("sftp.open_failed", err = "timeout").into_owned());
                }
                event = ch.next_event() => match event {
                    ShellEvent::Data(data) => {
                        buf.extend_from_slice(&data);
                        if marker_line_end(&buf, SFTP_READY_MARKER).is_some() {
                            hop.commit(&ui.shared).await;
                            drop(_guard);
                            let client = scell_vfs::SftpClient::with_owner(ch.into_stream(), link)
                                .await
                                .map_err(|e| t!("sftp.handshake_failed", err = e).into_owned())?;
                            return Ok(Arc::new(client) as Arc<dyn scell_vfs::Vfs>);
                        }
                        let clean = strip_ansi(&Encoding::UTF_8.decode(&buf));
                        if is_password_prompt(&clean) {
                            buf.clear();
                            let Some(pw) = hop
                                .clone()
                                .switch_password(method, user, &login_user, &host_label)
                                .await
                            else {
                                return Err(t!("sftp.open_failed", err = "cancelled").into_owned());
                            };
                            ch.write(format!("{pw}\r").as_bytes())
                                .await
                                .map_err(|e| t!("sftp.open_failed", err = e).into_owned())?;
                            deadline = Instant::now() + STEP_TIMEOUT;
                        }
                    }
                    ShellEvent::ExitStatus(_) => {}
                    ShellEvent::Closed => {
                        let clean = strip_ansi(&Encoding::UTF_8.decode(&buf));
                        if attempt + 1 < MAX_SWITCH_TRIES
                            && is_auth_failure(&clean)
                            && hop.has_asked()
                        {
                            break;
                        }
                        let reason = clean.trim();
                        let err = if reason.is_empty() {
                            "sftp-server exited".to_owned()
                        } else {
                            reason.to_owned()
                        };
                        return Err(t!("sftp.open_failed", err = err).into_owned());
                    }
                }
            }
        }
    }
    Err(t!("sftp.open_failed", err = "authentication failed").into_owned())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 标记独占一行并已收到行尾时，返回该行之后的位置。
fn marker_line_end(buf: &[u8], marker: &[u8]) -> Option<usize> {
    let end = find(buf, marker)? + marker.len();
    let rest = &buf[end..];
    if rest.starts_with(b"\n") {
        Some(end + 1)
    } else if rest.starts_with(b"\r\n") {
        Some(end + 2)
    } else {
        None
    }
}

/// 剥离 ANSI 转义序列与除 `\n` 外的控制字符，便于匹配提示符和期望文本。
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some('(' | ')' | '*' | '+' | '#' | '%') => {
                    chars.next();
                    chars.next();
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if c == '\n' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// 第一个非空行。切换用户的失败信息总在这一行，后面可能是目标用户 rc 文件的任意输出。
fn first_line(clean: &str) -> &str {
    clean
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

fn is_password_prompt(clean: &str) -> bool {
    let Some(last) = clean.trim_end().lines().last() else {
        return false;
    };
    let lower = last.trim().to_ascii_lowercase();
    (lower.contains("password") && lower.ends_with(':'))
        || ((lower.contains("密码") || lower.contains("口令"))
            && (lower.ends_with(':') || lower.ends_with('：')))
}

fn is_auth_failure(clean: &str) -> bool {
    let lower = clean.to_ascii_lowercase();
    lower.contains("authentication failure")
        || lower.contains("incorrect password")
        || lower.contains("sorry, try again")
        || lower.contains("permission denied")
        || lower.contains("not in the sudoers")
        || clean.contains("认证失败")
        || clean.contains("鉴定故障")
        || clean.contains("密码不正确")
        || clean.contains("对不起，请重试")
}

/// 末行是否像 shell 提示符。只看字符，分包到达的普通输出也可能命中，调用方需再等输出静默。
fn is_shell_prompt(clean: &str) -> bool {
    let trimmed = clean.trim_end_matches([' ', '\t']);
    let Some(last) = trimmed.lines().last() else {
        return false;
    };
    let last = last.trim();
    !last.is_empty()
        && !is_password_prompt(last)
        && !is_auth_failure(last)
        && last.ends_with(['#', '$', '%', '❯', '>'])
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_escape_sequences_and_control_chars() {
        let raw = "\x1b]0;user@host: ~\x07\x1b[01;32muser@host\x1b[00m:\x1b[01;34m~\x1b[00m$ \r";
        assert_eq!(strip_ansi(raw), "user@host:~$ ");
    }

    #[test]
    fn password_and_shell_prompt_detection() {
        assert!(is_password_prompt("Password: "));
        assert!(is_password_prompt("[sudo] password for ops: "));
        assert!(is_password_prompt("密码："));
        assert!(!is_password_prompt("user@host:~$ "));

        assert!(is_shell_prompt("user@host:~$ "));
        assert!(is_shell_prompt("root@host:/srv# "));
        assert!(!is_shell_prompt("Password: "));
        assert!(!is_shell_prompt("su: Authentication failure"));
    }

    #[test]
    fn auth_failure_only_on_first_line() {
        assert!(is_auth_failure(first_line(
            "\r\nsu: Authentication failure\nuser@h:~$ "
        )));
        let rc_noise = "\nroot@h:~# \n.bashrc: line 3: /x: Permission denied\nroot@h:~# ";
        assert!(!is_auth_failure(first_line(rc_noise)));
    }

    #[test]
    fn switch_marker_is_absent_from_command_echo() {
        let echo = b" printf '@@scell-%s\\n' switch; su - root\r\n";
        assert_eq!(find(echo, SWITCH_MARKER), None);
        assert_eq!(marker_line_end(b"x\r\n@@scell-switch", SWITCH_MARKER), None);
        assert_eq!(
            marker_line_end(b"x\r\n@@scell-switch\r\nPassword: ", SWITCH_MARKER),
            Some(19)
        );
    }

    #[test]
    fn auth_failure_and_shell_quoting() {
        assert!(is_auth_failure("su: Authentication failure\n"));
        assert!(is_auth_failure("Sorry, try again.\n"));
        assert!(!is_auth_failure("root@host:~# "));

        assert_eq!(shell_quote("root"), "root");
        assert_eq!(shell_quote("ops_1-a.b"), "ops_1-a.b");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
