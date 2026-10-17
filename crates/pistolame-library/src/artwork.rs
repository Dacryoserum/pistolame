//! How covers are named: by their content, so nothing keyed by a cover ever goes stale.

use md5::{Digest, Md5};

/// Names one stored cover: the lowercase hex MD5 of its bytes (deduplication, not security).
///
/// Identical images share one id and changed bytes get a new one, so anything keyed by it
/// (thumbnails on disk or in memory) never needs invalidating. The library hands these ids out
/// instead of the bytes: covers are read from the database only when one must be drawn.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtworkId(String);

impl ArtworkId {
    /// The id the library gives to a cover made of `bytes`.
    pub fn of_bytes(bytes: &[u8]) -> ArtworkId {
        use std::fmt::Write;
        let digest = Md5::digest(bytes);
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest.iter() {
            let _ = write!(hex, "{byte:02x}");
        }
        ArtworkId(hex)
    }

    /// An id read back from the database's `artworks.hash` column.
    pub(crate) fn from_stored(hash: String) -> ArtworkId {
        ArtworkId(hash)
    }

    /// The id as text: 32 lowercase hexadecimal digits, safe in a file name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_the_hex_md5_of_the_bytes() {
        // RFC 1321, appendix A.5: MD5("abc").
        assert_eq!(
            ArtworkId::of_bytes(b"abc").as_str(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(ArtworkId::of_bytes(b"abc"), ArtworkId::of_bytes(b"abc"));
        assert_ne!(ArtworkId::of_bytes(b"abc"), ArtworkId::of_bytes(b"abd"));
    }
}
