//! HPKE envelope (docs/PROTOCOL.md §2) — PINNED ciphersuite, matching
//! CryptoKit's Curve25519_HKDF_SHA256 / HKDF_SHA256 / ChaChaPoly exactly:
//! the iOS NSE cannot follow arbitrary Rust-side upgrades. Wire format:
//! base64(enc ‖ ct); aad binds the ciphertext to its collapse key so a
//! relay (or replayer) cannot re-route it under a different pane.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, Kem, OpModeS, Serializable};

pub const INFO: &[u8] = b"sigiltty-relay/v1/event";

pub fn seal(recipient_public_key_b64: &str, aad: &[u8], plaintext: &[u8]) -> Result<String, String> {
    let pk_bytes = BASE64
        .decode(recipient_public_key_b64)
        .map_err(|e| format!("device public key base64: {e}"))?;
    let pk = <X25519HkdfSha256 as Kem>::PublicKey::from_bytes(&pk_bytes)
        .map_err(|e| format!("device public key: {e}"))?;
    let (encapped, ciphertext) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256, _>(
        &OpModeS::Base,
        &pk,
        INFO,
        plaintext,
        aad,
        &mut rand::rngs::OsRng,
    )
    .map_err(|e| format!("hpke seal: {e}"))?;
    let mut wire = encapped.to_bytes().to_vec();
    wire.extend_from_slice(&ciphertext);
    Ok(BASE64.encode(wire))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hpke::OpModeR;

    #[test]
    fn roundtrips_against_an_hpke_recipient_with_bound_aad() {
        let (sk, pk) = X25519HkdfSha256::gen_keypair(&mut rand::rngs::OsRng);
        let pk_b64 = BASE64.encode(pk.to_bytes());
        let aad = b"herdr-server-w1:p4";

        let wire = seal(&pk_b64, aad, b"{\"status\":\"blocked\"}").unwrap();
        let bytes = BASE64.decode(wire).unwrap();
        let (enc_bytes, ct) = bytes.split_at(32);
        let enc = <X25519HkdfSha256 as Kem>::EncappedKey::from_bytes(enc_bytes).unwrap();

        let opened = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base, &sk, &enc, INFO, ct, aad,
        )
        .unwrap();
        assert_eq!(opened, b"{\"status\":\"blocked\"}");

        // The aad binding: opening under a different collapse key fails.
        let swapped = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base, &sk, &enc, INFO, ct, b"herdr-server-w9:p9",
        );
        assert!(swapped.is_err());
    }

    #[test]
    fn rejects_garbage_keys() {
        assert!(seal("not base64!!", b"aad", b"x").is_err());
        assert!(seal(&BASE64.encode([0u8; 7]), b"aad", b"x").is_err());
    }
}
