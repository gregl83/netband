use sha2::{Digest, Sha256};

/// Stable UUIDs keep fixture relationships readable without weakening production ID types.
pub fn id<T: std::str::FromStr>(label: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    let bytes: [u8; 16] = Sha256::digest(label.as_bytes())[..16].try_into().unwrap();
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
        .parse()
        .unwrap()
}
