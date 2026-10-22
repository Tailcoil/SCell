//! 非 UTF-8 会话的双向流式转码。UTF-8 会话直通，不经过解码器。

/// 会话字符编码，标签取自 WHATWG 编码标准（如 `gbk`、`gb18030`、`big5`、`shift_jis`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoding(&'static encoding_rs::Encoding);

impl Encoding {
    pub const UTF_8: Self = Self(encoding_rs::UTF_8);

    pub fn for_label(label: &str) -> Option<Self> {
        encoding_rs::Encoding::for_label(label.as_bytes()).map(Self)
    }

    pub fn name(self) -> &'static str {
        self.0.name()
    }

    /// 主机配置中的编码标签；缺省或无法识别时为 UTF-8。
    pub fn of_label(label: Option<&str>) -> Self {
        label.and_then(Self::for_label).unwrap_or_default()
    }

    /// 一次性解码（如文件名），无法解码的字节替换为 U+FFFD。
    pub fn decode(self, bytes: &[u8]) -> std::borrow::Cow<'_, str> {
        self.0.decode_without_bom_handling(bytes).0
    }

    /// 一次性编码（如文件名）；含该编码表示不了的字符时返回 `None`。
    pub fn encode(self, text: &str) -> Option<std::borrow::Cow<'_, [u8]>> {
        let (bytes, _, unmappable) = self.0.encode(text);
        (!unmappable).then_some(bytes)
    }

    fn is_utf8(self) -> bool {
        self.0 == encoding_rs::UTF_8
    }
}

impl Default for Encoding {
    fn default() -> Self {
        Self::UTF_8
    }
}

/// 输出方向的解码器。多字节字符跨 chunk 时由内部状态拼接。
pub(crate) struct Decoder {
    inner: Option<encoding_rs::Decoder>,
    buf: String,
}

impl Decoder {
    pub(crate) fn new(encoding: Encoding) -> Self {
        Self {
            inner: (!encoding.is_utf8()).then(|| encoding.0.new_decoder_without_bom_handling()),
            buf: String::new(),
        }
    }

    /// 返回可直接喂给终端的 UTF-8 字节。
    pub(crate) fn decode<'a>(&'a mut self, input: &'a [u8]) -> &'a [u8] {
        let Some(decoder) = &mut self.inner else {
            return input;
        };
        self.buf.clear();
        let needed = decoder
            .max_utf8_buffer_length(input.len())
            .unwrap_or(input.len() * 3);
        self.buf.reserve(needed);
        let (_, _, _) = decoder.decode_to_string(input, &mut self.buf, false);
        self.buf.as_bytes()
    }
}

/// 输入方向的编码器。输入是终端编码后的 UTF-8 字节（转义序列均为 ASCII，转码后不变）。
pub(crate) struct Encoder {
    encoding: Encoding,
}

impl Encoder {
    pub(crate) fn new(encoding: Encoding) -> Self {
        Self { encoding }
    }

    pub(crate) fn encode(&self, input: Vec<u8>) -> Vec<u8> {
        if self.encoding.is_utf8() {
            return input;
        }
        let text = String::from_utf8_lossy(&input);
        let (bytes, _, _) = self.encoding.0.encode(&text);
        bytes.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gbk_decodes_across_chunk_boundary() {
        let gbk = Encoding::for_label("gbk").expect("gbk");
        let (bytes, _, _) = encoding_rs::GBK.encode("中文\x1b[0m");
        let mut decoder = Decoder::new(gbk);
        let mut out = Vec::new();
        // 在第一个汉字的两个字节之间切开
        out.extend_from_slice(decoder.decode(&bytes[..1]));
        out.extend_from_slice(decoder.decode(&bytes[1..]));
        assert_eq!(String::from_utf8(out).expect("utf8"), "中文\x1b[0m");
    }

    #[test]
    fn utf8_is_passthrough() {
        let mut decoder = Decoder::new(Encoding::UTF_8);
        assert_eq!(decoder.decode(b"\xe4\xb8"), b"\xe4\xb8");
        assert_eq!(Encoder::new(Encoding::UTF_8).encode(b"x".to_vec()), b"x");
    }

    #[test]
    fn gbk_encodes_input() {
        let enc = Encoder::new(Encoding::for_label("gbk").expect("gbk"));
        assert_eq!(enc.encode("中".as_bytes().to_vec()), vec![0xd6, 0xd0]);
    }
}
