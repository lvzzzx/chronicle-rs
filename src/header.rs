use crate::{Error, Result};

#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageHeader {
    // Field order avoids implicit padding; serialization uses explicit offsets.
    pub seq: u64,
    pub timestamp_ns: u64,
    pub length: u32,
    pub checksum: u32,
    pub type_id: u16,
    pub flags: u8,
    pub _reserved: u8,
    pub _pad: [u8; 36],
}

impl MessageHeader {
    pub fn new(length: u32, seq: u64, timestamp_ns: u64, type_id: u16, checksum: u32) -> Self {
        Self {
            seq,
            timestamp_ns,
            length,
            checksum,
            type_id,
            flags: 0,
            _reserved: 0,
            _pad: [0u8; 36],
        }
    }

    pub fn to_bytes(&self) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0..4].copy_from_slice(&self.length.to_le_bytes());
        buf[4..12].copy_from_slice(&self.seq.to_le_bytes());
        buf[12..20].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        buf[20] = self.flags;
        buf[21..23].copy_from_slice(&self.type_id.to_le_bytes());
        buf[23] = self._reserved;
        buf[24..28].copy_from_slice(&self.checksum.to_le_bytes());
        buf[28..64].copy_from_slice(&self._pad);
        buf
    }

    pub fn from_bytes(bytes: &[u8; 64]) -> Result<Self> {
        let length = u32::from_le_bytes(bytes[0..4].try_into().expect("slice length"));
        let seq = u64::from_le_bytes(bytes[4..12].try_into().expect("slice length"));
        let timestamp_ns =
            u64::from_le_bytes(bytes[12..20].try_into().expect("slice length"));
        let flags = bytes[20];
        let type_id = u16::from_le_bytes(bytes[21..23].try_into().expect("slice length"));
        let _reserved = bytes[23];
        let checksum = u32::from_le_bytes(bytes[24..28].try_into().expect("slice length"));
        let mut _pad = [0u8; 36];
        _pad.copy_from_slice(&bytes[28..64]);
        Ok(Self {
            length,
            seq,
            timestamp_ns,
            flags,
            type_id,
            _reserved,
            checksum,
            _pad,
        })
    }

    pub fn crc32(payload: &[u8]) -> u32 {
        use crc32fast::Hasher;
        let mut hasher = Hasher::new();
        hasher.update(payload);
        hasher.finalize()
    }

    pub fn validate_crc(&self, payload: &[u8]) -> Result<()> {
        let expected = Self::crc32(payload);
        if expected == self.checksum {
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
