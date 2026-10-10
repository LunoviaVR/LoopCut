//! Opaque media tokens: the only handle the webview ever receives for a file.
//!
//! A token is 128 random bits written as 32 lowercase hex digits. It maps to a canonical path inside Rust; the
//! webview never sees or sends a filesystem path to a media protocol. The randomness comes from the standard
//! library only: `RandomState` is seeded from the operating system, and each token mixes that seed with the clock,
//! a process-wide counter and the process id. That is ample for an unguessable local handle; it is not a
//! cryptographic key.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// Length of a token in characters.
pub const TOKEN_LEN: usize = 32;

static COUNTER: AtomicU64 = AtomicU64::new(1);

/// One 64-bit hash of process-local entropy. `salt` separates the two halves of a token.
fn hash_word(salt: u64) -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(salt);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    hasher.write_u128(nanos);
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    // The address of a static differs between runs under ASLR.
    hasher.write_usize(std::ptr::addr_of!(COUNTER) as usize);
    hasher.finish()
}

/// A fresh 128-bit token as 32 lowercase hex digits.
pub fn new_token() -> String {
    format!(
        "{:016x}{:016x}",
        hash_word(0x6c6f_6f70_6375_7431),
        hash_word(0x6c6f_6f70_6375_7432)
    )
}

/// Eight hex digits for temporary file names (uniqueness, not secrecy, is what matters there).
pub fn random_hex_suffix() -> String {
    format!("{:08x}", hash_word(0x7465_6d70_0000_0001) as u32)
}

/// True for a well-formed token: exactly 32 lowercase hex digits. Anything else is refused before a lookup.
pub fn is_token(text: &str) -> bool {
    text.len() == TOKEN_LEN
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Path table: token to canonical path, with the reverse index so one file keeps one token.
#[derive(Default)]
pub struct TokenTable {
    inner: Mutex<Maps>,
}

#[derive(Default)]
struct Maps {
    by_token: HashMap<String, PathBuf>,
    by_path: HashMap<PathBuf, String>,
}

impl TokenTable {
    /// Returns the token for `canonical`, minting one if the file has none yet. `canonical` must already be
    /// canonicalized by the caller (see `commands::media::canonical_path`).
    pub fn mint(&self, canonical: &Path) -> String {
        let mut maps = self.inner.lock();
        if let Some(existing) = maps.by_path.get(canonical) {
            return existing.clone();
        }
        let token = loop {
            let candidate = new_token();
            if !maps.by_token.contains_key(&candidate) {
                break candidate;
            }
        };
        maps.by_token.insert(token.clone(), canonical.to_path_buf());
        maps.by_path.insert(canonical.to_path_buf(), token.clone());
        token
    }

    /// The canonical path behind `token`, or `None` for an unknown, released or malformed token.
    pub fn resolve(&self, token: &str) -> Option<PathBuf> {
        if !is_token(token) {
            return None;
        }
        self.inner.lock().by_token.get(token).cloned()
    }

    /// Forgets `token`. Later requests for it fail with "not found".
    pub fn release(&self, token: &str) {
        let mut maps = self.inner.lock();
        if let Some(path) = maps.by_token.remove(token) {
            maps.by_path.remove(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_32_lowercase_hex() {
        for _ in 0..200 {
            let t = new_token();
            assert!(is_token(&t), "{t}");
        }
    }

    #[test]
    fn tokens_do_not_repeat() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(new_token()));
        }
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        assert!(!is_token(""));
        assert!(!is_token(&"A".repeat(TOKEN_LEN)));
        assert!(!is_token(&"g".repeat(TOKEN_LEN)));
        assert!(!is_token(&"0".repeat(TOKEN_LEN - 1)));
        assert!(!is_token(&"0".repeat(TOKEN_LEN + 1)));
        assert!(!is_token("../../../../Windows/win.ini"));
        assert!(is_token("0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn mint_reuses_a_token_for_the_same_path() {
        let table = TokenTable::default();
        let path = Path::new("C:\\videos\\a.mp4");
        let first = table.mint(path);
        assert_eq!(table.mint(path), first);
        assert_eq!(table.resolve(&first).as_deref(), Some(path));
        assert_eq!(table.resolve(&first).as_deref(), Some(path));
    }

    #[test]
    fn distinct_paths_get_distinct_tokens() {
        let table = TokenTable::default();
        let a = table.mint(Path::new("C:\\a.mp4"));
        let b = table.mint(Path::new("C:\\b.mp4"));
        assert_ne!(a, b);
    }

    #[test]
    fn resolve_rejects_unknown_and_released_tokens() {
        let table = TokenTable::default();
        assert_eq!(table.resolve(&"0".repeat(TOKEN_LEN)), None);
        assert_eq!(table.resolve("../etc"), None);
        let token = table.mint(Path::new("C:\\clip.mov"));
        table.release(&token);
        assert_eq!(table.resolve(&token), None);
        assert_eq!(table.resolve(&token), None);
        // Releasing again is harmless.
        table.release(&token);
    }
}
