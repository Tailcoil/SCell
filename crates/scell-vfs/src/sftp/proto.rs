//! SFTP v3 报文（draft-ietf-secsh-filexfer-02）。所有整数为大端，字符串为 u32 长度前缀的原始字节。

use bytes::{BufMut, Bytes, BytesMut};

use crate::{Metadata, Result, VfsError};

pub(crate) const VERSION: u32 = 3;

pub(crate) const FXP_INIT: u8 = 1;
pub(crate) const FXP_VERSION: u8 = 2;
pub(crate) const FXP_OPEN: u8 = 3;
pub(crate) const FXP_CLOSE: u8 = 4;
pub(crate) const FXP_READ: u8 = 5;
pub(crate) const FXP_WRITE: u8 = 6;
pub(crate) const FXP_LSTAT: u8 = 7;
pub(crate) const FXP_SETSTAT: u8 = 9;
pub(crate) const FXP_OPENDIR: u8 = 11;
pub(crate) const FXP_READDIR: u8 = 12;
pub(crate) const FXP_REMOVE: u8 = 13;
pub(crate) const FXP_MKDIR: u8 = 14;
pub(crate) const FXP_RMDIR: u8 = 15;
pub(crate) const FXP_REALPATH: u8 = 16;
pub(crate) const FXP_STAT: u8 = 17;
pub(crate) const FXP_RENAME: u8 = 18;
pub(crate) const FXP_EXTENDED: u8 = 200;

pub(crate) const FXP_STATUS: u8 = 101;
pub(crate) const FXP_HANDLE: u8 = 102;
pub(crate) const FXP_DATA: u8 = 103;
pub(crate) const FXP_NAME: u8 = 104;
pub(crate) const FXP_ATTRS: u8 = 105;

pub(crate) const FXF_READ: u32 = 0x01;
pub(crate) const FXF_WRITE: u32 = 0x02;
pub(crate) const FXF_CREAT: u32 = 0x08;
pub(crate) const FXF_TRUNC: u32 = 0x10;
pub(crate) const FXF_EXCL: u32 = 0x20;

const ATTR_SIZE: u32 = 0x01;
const ATTR_UIDGID: u32 = 0x02;
const ATTR_PERMISSIONS: u32 = 0x04;
const ATTR_ACMODTIME: u32 = 0x08;
const ATTR_EXTENDED: u32 = 0x8000_0000;

pub(crate) const FX_OK: u32 = 0;
pub(crate) const FX_EOF: u32 = 1;
const FX_NO_SUCH_FILE: u32 = 2;
const FX_PERMISSION_DENIED: u32 = 3;
const FX_OP_UNSUPPORTED: u32 = 8;

/// 接收报文的长度上限，防止异常长度导致巨量分配。OpenSSH 的上限为 256 KiB。
pub(crate) const MAX_PACKET: usize = 4 * 1024 * 1024;

/// 组装一个报文；`finish` 时回填长度。
pub(crate) struct Encoder(BytesMut);

impl Encoder {
    pub(crate) fn new(kind: u8) -> Self {
        let mut buf = BytesMut::with_capacity(64);
        buf.put_u32(0);
        buf.put_u8(kind);
        Self(buf)
    }

    pub(crate) fn request(kind: u8, id: u32) -> Self {
        let mut e = Self::new(kind);
        e.u32(id);
        e
    }

    pub(crate) fn u32(&mut self, v: u32) -> &mut Self {
        self.0.put_u32(v);
        self
    }

    pub(crate) fn u64(&mut self, v: u64) -> &mut Self {
        self.0.put_u64(v);
        self
    }

    pub(crate) fn string(&mut self, s: &[u8]) -> &mut Self {
        self.0.put_u32(s.len() as u32);
        self.0.put_slice(s);
        self
    }

    pub(crate) fn attrs(&mut self, m: &Metadata) -> &mut Self {
        let mut flags = 0;
        if m.size.is_some() {
            flags |= ATTR_SIZE;
        }
        if m.uid.is_some() && m.gid.is_some() {
            flags |= ATTR_UIDGID;
        }
        if m.permissions.is_some() {
            flags |= ATTR_PERMISSIONS;
        }
        // v3 的访问与修改时间只能同时设置，缺一个时用另一个补上
        let times = match (m.accessed, m.modified) {
            (None, None) => None,
            (a, b) => Some((a.or(b).unwrap_or(0), b.or(a).unwrap_or(0))),
        };
        if times.is_some() {
            flags |= ATTR_ACMODTIME;
        }
        self.u32(flags);
        if let Some(size) = m.size {
            self.u64(size);
        }
        if let (Some(uid), Some(gid)) = (m.uid, m.gid) {
            self.u32(uid).u32(gid);
        }
        if let Some(p) = m.permissions {
            self.u32(p);
        }
        if let Some((a, b)) = times {
            // v3 的时间是 32 位
            self.u32(a as u32).u32(b as u32);
        }
        self
    }

    pub(crate) fn finish(mut self) -> Bytes {
        let len = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&len.to_be_bytes());
        self.0.freeze()
    }
}

/// 解析报文内容。越界时返回协议错误而不是 panic。
pub(crate) struct Decoder<'a>(&'a [u8]);

fn truncated() -> VfsError {
    VfsError::Protocol("truncated packet".into())
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self(buf)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(truncated());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok((u64::from(self.u32()?) << 32) | u64::from(self.u32()?))
    }

    pub(crate) fn string(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    pub(crate) fn attrs(&mut self) -> Result<Metadata> {
        let flags = self.u32()?;
        let mut m = Metadata::default();
        if flags & ATTR_SIZE != 0 {
            m.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            m.uid = Some(self.u32()?);
            m.gid = Some(self.u32()?);
        }
        if flags & ATTR_PERMISSIONS != 0 {
            m.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            m.accessed = Some(u64::from(self.u32()?));
            m.modified = Some(u64::from(self.u32()?));
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.string()?;
                self.string()?;
            }
        }
        Ok(m)
    }
}

/// STATUS 报文中的错误码转为错误；`FX_OK` 与 `FX_EOF` 由调用方先行处理。
pub(crate) fn status_error(code: u32, message: &[u8]) -> VfsError {
    match code {
        FX_NO_SUCH_FILE => VfsError::NotFound,
        FX_PERMISSION_DENIED => VfsError::PermissionDenied,
        FX_OP_UNSUPPORTED => VfsError::Unsupported,
        _ => {
            let message = String::from_utf8_lossy(message);
            VfsError::Failure(if message.is_empty() {
                rust_i18n::t!("error.status", code = code).into_owned()
            } else {
                message.into_owned()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attrs_round_trip() {
        let m = Metadata {
            size: Some(1 << 40),
            uid: Some(1000),
            gid: Some(100),
            permissions: Some(0o100_644),
            accessed: Some(1_700_000_000),
            modified: Some(1_700_000_001),
        };
        let mut e = Encoder::new(FXP_ATTRS);
        e.attrs(&m);
        let packet = e.finish();
        assert_eq!(
            u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize,
            packet.len() - 4
        );
        let mut d = Decoder::new(&packet[5..]);
        assert_eq!(d.attrs().unwrap(), m);
        assert!(d.is_empty());

        // 只设修改时间时，访问时间取同一个值
        let mut e = Encoder::new(FXP_ATTRS);
        e.attrs(&Metadata {
            modified: Some(5),
            ..Default::default()
        });
        let packet = e.finish();
        let got = Decoder::new(&packet[5..]).attrs().unwrap();
        assert_eq!((got.accessed, got.modified), (Some(5), Some(5)));
    }

    #[test]
    fn truncated_input_is_an_error() {
        let mut d = Decoder::new(&[0, 0, 0, 9, b'a']);
        assert!(matches!(d.string(), Err(VfsError::Protocol(_))));
    }
}
