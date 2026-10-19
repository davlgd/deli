//! OpenSSH public key lines, checked for their structure: types sshd accepts,
//! wire format, sizes. Whether the key itself is sound (an ECDSA point on
//! its curve) is left to `ssh-keygen`, which preflight runs on every key: an
//! installation that erases a disk and then refuses the only key it was
//! given locks everyone out.
//!
//! A line is `<type> <base64 blob> [comment]`; the blob is the key in SSH
//! wire format (length-prefixed strings), whose first string repeats the type.

/// Whether `line` is one complete, well-formed public key of a type sshd accepts.
pub fn is_valid(line: &str) -> bool {
    if line.contains(['\n', '\r']) {
        return false;
    }
    let mut fields = line.split_whitespace();
    let (Some(kind), Some(blob)) = (fields.next(), fields.next()) else {
        return false;
    };
    let Some(bytes) = decode_base64(blob) else {
        return false;
    };
    let mut wire = Wire(&bytes);
    if wire.string() != Some(kind.as_bytes()) {
        return false;
    }
    let body_ok = match kind {
        "ssh-ed25519" => wire.string().is_some_and(|k| k.len() == 32),
        "sk-ssh-ed25519@openssh.com" => {
            wire.string().is_some_and(|k| k.len() == 32) && wire.string().is_some()
        }
        // Exponent, then a modulus of at least 2048 bits.
        "ssh-rsa" => {
            wire.string().is_some_and(|e| !e.is_empty())
                && wire.string().is_some_and(|n| n.len() >= 256)
        }
        "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" | "ecdsa-sha2-nistp521" => {
            let curve = &kind["ecdsa-sha2-".len()..];
            wire.string() == Some(curve.as_bytes())
                && wire.string().is_some_and(|q| uncompressed_point(q, curve))
        }
        "sk-ecdsa-sha2-nistp256@openssh.com" => {
            wire.string() == Some(b"nistp256".as_slice())
                && wire
                    .string()
                    .is_some_and(|q| uncompressed_point(q, "nistp256"))
                && wire.string().is_some()
        }
        _ => false,
    };
    body_ok && wire.0.is_empty()
}

/// 0x04 then both coordinates, at the curve's exact size. Whether the point
/// is on the curve is left to `ssh-keygen`, which preflight runs on the live
/// system.
fn uncompressed_point(q: &[u8], curve: &str) -> bool {
    let coordinate = match curve {
        "nistp256" => 32,
        "nistp384" => 48,
        "nistp521" => 66,
        _ => return false,
    };
    q.len() == 1 + 2 * coordinate && q[0] == 4
}

struct Wire<'a>(&'a [u8]);

impl<'a> Wire<'a> {
    fn string(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(u32::from_be_bytes(self.0.get(..4)?.try_into().ok()?)).ok()?;
        let value = self.0.get(4..4 + len)?;
        self.0 = &self.0[4 + len..];
        Some(value)
    }
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let bytes = text.trim_end_matches('=').as_bytes();
    if bytes.len() % 4 == 1 || text.len() - bytes.len() > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= u32::from(value(c)?) << (18 - 6 * i);
        }
        out.extend_from_slice(&acc.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM7w6tvHPjMHe2grkjvMyevSUvyZE/gsp1Xnad41MMU0 ops@example";

    #[test]
    fn real_key_passes() {
        assert!(is_valid(ED25519));
        assert!(is_valid(ED25519.rsplit_once(' ').unwrap().0));
    }

    #[test]
    fn broken_keys_fail() {
        for bad in [
            "not-an-ssh-key",
            // Type header only, no key material.
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5",
            // Blob of another type.
            "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIM7w6tvHPjMHe2grkjvMyevSUvyZE/gsp1Xnad41MMU0",
            // Truncated key material.
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM7w6tvHPjMHe2grkjvMyevSUvyZE/gsp1Xnad41",
            // A second line smuggled in.
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM7w6tvHPjMHe2grkjvMyevSUvyZE/gsp1Xnad41MMU0\nssh-ed25519 AAAA",
            "ssh-ed25519 !!!!",
            // ECDSA with the point marker only.
            "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAAABBA==",
        ] {
            assert!(!is_valid(bad), "accepted: {bad}");
        }
    }
}
