//! The one SHA-256 spelling the harness uses for fingerprints, journal
//! identities and scope digests. Every site hashes through here so a digest
//! computed by the runner compares equal to the daemon's.
use sha2::{Digest, Sha256};

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

/// Lower-case hex SHA-256 of the compact JSON serialisation of `value`.
pub fn sha256_json(value: &impl serde::Serialize) -> anyhow::Result<String> {
    Ok(sha256_hex(serde_json::to_vec(value)?))
}
