use std::sync::atomic::{AtomicU32, Ordering};

use crate::{Error, Result};

pub const HEADER_SIZE: usize = 64;
pub const RECORD_ALIGN: usize = 64;
pub const MAX_PAYLOAD_LEN: usize = u32::MAX as usize - 1;
pub const PAD_TYPE_ID: u16 = 0xFFFF;

pub const COMMIT_LEN_OFFSET: usize = 0;
pub const SEQ_OFFSET: usize = 4;
pub const TIMESTAMP_OFFSET: usize = 12;
pub const TYPE_ID_OFFSET: usize = 20;
pub const FLAGS_OFFSET: usize = 22;
pub const RESERVED_OFFSET: usize = 24;

#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageHeader {
    /// Commit word:
    /// 0 = uncommitted
    /// >0 = committed payload length + 1
    pub commit_len: u32,
    pub _pad0: u32,
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub flags: u16,
    pub reserved_u32: u32,
    pub _pad: [u8; 32],
}

impl MessageHeader {
    pub fn new_uncommitted(seq: u64, timestamp_ns: u64, type_id: u16, flags: u16, reserved_u32: u32) -> Self {
        Self {
            commit_len: 0,
            _pad0: 0,
            seq,
            timestamp_ns,
            type_id,
            flags,
            reserved_u32,
            _pad: [0u8; 32],
        }
    }

    pub fn to_bytes(&self) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0..4].copy_from_slice(&self.commit_len.to_le_bytes());
        buf[4..12].copy_from_slice(&self.seq.to_le_bytes());
        buf[12..20].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf[20..22].copy_from_slice(&self.type_id.to_le_bytes());
        buf[22..24].copy_from_slice(&self.flags.to_le_bytes());
        buf[24..28].copy_from_slice(&self.reserved_u32.to_le_bytes());
        buf[28..60].copy_from_slice(&self._pad);
        buf
    }

    pub fn from_bytes(bytes: &[u8; 64]) -> Result<Self> {
        let commit_len = u32::from_le_bytes(bytes[0..4].try_into().expect("slice length"));
        let seq = u64::from_le_bytes(bytes[4..12].try_into().expect("slice length"));
        let timestamp_ns = u64::from_le_bytes(bytes[12..20].try_into().expect("slice length"));
        let type_id = u16::from_le_bytes(bytes[20..22].try_into().expect("slice length"));
        let flags = u16::from_le_bytes(bytes[22..24].try_into().expect("slice length"));
        let reserved_u32 = u32::from_le_bytes(bytes[24..28].try_into().expect("slice length"));
        let mut _pad = [0u8; 32];
        _pad.copy_from_slice(&bytes[28..60]);
        Ok(Self {
            commit_len,
            _pad0: 0,
            seq,
            timestamp_ns,
            type_id,
            flags,
            reserved_u32,
            _pad,
        })
    }

    pub fn commit_len_for_payload(payload_len: usize) -> Result<u32> {
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(Error::PayloadTooLarge);
        }
        Ok((payload_len as u32) + 1)
    }

    pub fn payload_len_from_commit(commit_len: u32) -> Result<usize> {
        if commit_len == 0 {
            return Err(Error::Corrupt("commit length is zero"));
        }
        Ok((commit_len - 1) as usize)
    }

    pub fn load_commit_len(ptr: *const u8) -> u32 {
        // SAFETY: commit_len is at offset 0 and header is 64-byte aligned.
        let atomic = unsafe { &*(ptr as *const AtomicU32) };
        atomic.load(Ordering::Acquire)
    }

    pub fn store_commit_len(ptr: *mut u8, commit_len: u32) {
        // SAFETY: commit_len is at offset 0 and header is 64-byte aligned.
        let atomic = unsafe { &*(ptr as *const AtomicU32) };
        atomic.store(commit_len, Ordering::Release);
    }

    pub fn crc32(payload: &[u8]) -> u32 {
        use crc32fast::Hasher;
        let mut hasher = Hasher::new();
        hasher.update(payload);
        hasher.finalize()
    }

    pub fn validate_crc(&self, payload: &[u8]) -> Result<()> {
        let expected = Self::crc32(payload);
        if expected == self.reserved_u32 {
            Ok(())
        } else {
            Err(Error::Corrupt("crc mismatch"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MessageHeader;
    use std::mem::{align_of, size_of};

    #[test]
    fn header_size_and_alignment() {
        assert_eq!(size_of::<MessageHeader>(), 64);
        assert_eq!(align_of::<MessageHeader>(), 64);
    }

    #[test]
    fn crc_matches_known_payload() {
        let crc = MessageHeader::crc32(b"hello");
        assert_eq!(crc, 0x3610A686);
    }
}
