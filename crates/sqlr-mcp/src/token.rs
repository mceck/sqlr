//! Token generation and constant-time comparison. The value is never logged.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::error::{ToolError, ToolResult};

/// 256 bits of OS randomness as unpadded base64url.
pub fn generate_token() -> ToolResult<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| ToolError::internal(format!("no OS randomness available: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Compares two byte strings without early exit on the first difference.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut difference = a.len() ^ b.len();
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        difference |= usize::from(x ^ y);
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_distinct() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(!a.contains('='));
    }

    #[test]
    fn comparison_handles_length_and_content() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
