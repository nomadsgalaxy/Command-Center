//! The server half of the agent's login (docs/agent.md §2). After the TLS handshake the host sends a
//! single-use challenge, and the Frame answers with its name and an Ed25519 signature over `signed()`.
//! I check that against the exact key in trusted-frames/<name>.pub. An unknown name and a wrong
//! signature look the same from outside (closed, no reason) so nobody can use this to find out which
//! Frames exist.
use crate::agent::{canon, signed, VERSION};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub const AUTH_MAX: usize = 256;
pub const AUTH_S: Duration = Duration::from_secs(5);

/// Whether this is a Frame name H1 allows: `[a-z0-9][a-z0-9-]{0,31}`.
pub fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty() && b.len() <= 32 && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Reads one line straight off the stream, at most `limit` bytes before its newline, within
/// `timeout` (the caller sets the socket's read timeout). Returns the line and whatever came after it.
pub fn read_line(s: &mut impl Read, limit: usize, timeout: Duration) -> Option<(Vec<u8>, Vec<u8>)> {
    let end = Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        if let Some(i) = buf.iter().position(|&b| b == b'\n') {
            let rest = buf.split_off(i + 1);
            buf.pop();
            return Some((buf, rest));
        }
        if buf.len() > limit || Instant::now() > end {
            return None;
        }
        let mut chunk = [0u8; 512];
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Sends the challenge, reads the answer and checks it. Gives back the Frame's name (its key matched)
/// plus any bytes read past its line, or None for anything else.
pub fn authenticate(s: &mut (impl Read + Write), host_pk: &[u8; 32], trusted_frames: &Path, challenge: [u8; 32]) -> Option<(String, Vec<u8>)> {
    let mut hello = canon(&json!({"v": VERSION, "challenge": B64.encode(challenge)}));
    hello.push(b'\n');
    s.write_all(&hello).ok()?;
    s.flush().ok()?;
    let (line, rest) = read_line(s, AUTH_MAX, AUTH_S)?;
    let msg: Value = serde_json::from_slice(&line).ok()?;
    let obj = msg.as_object()?;
    if obj.len() != 2 {
        return None;
    }
    let frame = obj.get("frame")?.as_str()?;
    if !valid_name(frame) {
        return None; // check this before touching any file (H1)
    }
    let sig: [u8; 64] = B64.decode(obj.get("sig")?.as_str()?).ok()?.try_into().ok()?;
    let pub_b64 = std::fs::read_to_string(trusted_frames.join(format!("{frame}.pub"))).ok()?;
    let pk: [u8; 32] = B64.decode(pub_b64.trim()).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&pk).ok()?.verify(&signed(&challenge, host_pk, frame), &Signature::from_bytes(&sig)).ok()?;
    s.write_all(b"{\"ok\":true}\n").ok()?;
    s.flush().ok()?;
    Some((frame.to_owned(), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("steam-frame") && valid_name("0") && !valid_name("-x") && !valid_name("A") && !valid_name("../x") && !valid_name(&"a".repeat(33)));
    }
}
