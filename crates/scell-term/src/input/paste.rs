use super::InputMode;

/// 编码粘贴内容：换行统一为 CR；括号粘贴模式下去掉 ESC，防止内容提前结束粘贴并注入命令。
pub fn encode_paste(text: &str, mode: InputMode) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if mode.bracketed_paste {
        let mut out = Vec::with_capacity(normalized.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend(normalized.bytes().filter(|&b| b != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalized.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_strips_escape() {
        let mode = InputMode {
            bracketed_paste: true,
            ..Default::default()
        };
        assert_eq!(
            encode_paste("a\n\x1b[201~rm", mode),
            b"\x1b[200~a\r[201~rm\x1b[201~".to_vec()
        );
    }

    #[test]
    fn plain_paste_normalizes_newlines() {
        assert_eq!(
            encode_paste("a\r\nb\nc", InputMode::default()),
            b"a\rb\rc".to_vec()
        );
    }
}
