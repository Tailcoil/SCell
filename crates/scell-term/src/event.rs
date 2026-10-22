/// 终端在解析过程中产生的事件，由会话层消费。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TermEvent {
    /// 必须写回传输层的字节（DA、DSR、颜色查询等的应答，剪贴板读取结果）。
    PtyWrite(Vec<u8>),
    Title(String),
    ResetTitle,
    Bell,
    /// OSC 52 写剪贴板。读取请求在门面内按配置直接应答或忽略。
    ClipboardStore(ClipboardKind, String),
    /// OSC 7 上报的工作目录，原样保留 `file://host/path` 形式。
    WorkingDirectory(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardKind {
    Clipboard,
    Selection,
}
