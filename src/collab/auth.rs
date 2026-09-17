use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;
pub const MIN_PASSPHRASE_LEN: usize = 8;
pub const MAX_PASSPHRASE_BYTES: usize = 1024;
const MAX_PASSPHRASE_HASH_BYTES: usize = 512;

fn argon2id() -> anyhow::Result<Argon2<'static>> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        None,
    )
    .map_err(|err| anyhow::anyhow!("failed to build Argon2 params: {err}"))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub fn normalize_optional_passphrase(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub fn validate_passphrase(passphrase: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        passphrase.len() <= MAX_PASSPHRASE_BYTES,
        "Passphrase too long"
    );
    if passphrase.chars().count() < MIN_PASSPHRASE_LEN {
        anyhow::bail!("La passphrase tiene que tener al menos {MIN_PASSPHRASE_LEN} caracteres");
    }
    Ok(())
}

pub fn hash_passphrase(passphrase: &str) -> anyhow::Result<String> {
    validate_passphrase(passphrase)?;
    let salt = SaltString::generate(&mut OsRng);
    let hash = argon2id()?
        .hash_password(passphrase.as_bytes(), &salt)
        .map_err(|err| anyhow::anyhow!("failed to hash session passphrase: {err}"))?;
    Ok(hash.to_string())
}

/// Compares two secrets without short-circuiting so the comparison time does
/// not leak how many leading characters matched.
pub fn constant_time_str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().min(b.len()) {
        diff |= usize::from(a[i] ^ b[i]);
    }
    diff == 0
}

/// Validate the PHC policy before any memory-hard work. PasswordVerifier
/// deliberately uses the parameters in the PHC string, not our local defaults.
pub fn validate_passphrase_hash(hash: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        hash.len() <= MAX_PASSPHRASE_HASH_BYTES,
        "Passphrase hash too long"
    );
    let parsed =
        PasswordHash::new(hash).map_err(|err| anyhow::anyhow!("invalid passphrase hash: {err}"))?;
    anyhow::ensure!(
        parsed.algorithm.as_str() == "argon2id" && parsed.version == Some(19),
        "Unsupported passphrase algorithm or version"
    );
    let params = Params::try_from(&parsed)
        .map_err(|err| anyhow::anyhow!("invalid passphrase parameters: {err}"))?;
    anyhow::ensure!(
        params.m_cost() == ARGON2_MEMORY_KIB
            && params.t_cost() == ARGON2_ITERATIONS
            && params.p_cost() == ARGON2_PARALLELISM
            && params.keyid().is_empty()
            && params.data().is_empty(),
        "Passphrase parameters do not match the bounded server policy"
    );
    anyhow::ensure!(
        parsed
            .hash
            .as_ref()
            .is_some_and(|output| output.as_bytes().len() == 32),
        "Passphrase hash must have a 32-byte output"
    );
    let salt = parsed
        .salt
        .ok_or_else(|| anyhow::anyhow!("Passphrase salt missing"))?;
    let mut bytes = [0u8; 64];
    let salt = salt
        .decode_b64(&mut bytes)
        .map_err(|err| anyhow::anyhow!("invalid passphrase salt: {err}"))?;
    anyhow::ensure!(salt.len() >= 16, "Passphrase salt too short");
    Ok(())
}

pub fn verify_passphrase(hash: &str, passphrase: &str) -> anyhow::Result<bool> {
    validate_passphrase_hash(hash)?;
    anyhow::ensure!(
        passphrase.len() <= MAX_PASSPHRASE_BYTES,
        "Passphrase too long"
    );
    let parsed = PasswordHash::new(hash)
        .map_err(|err| anyhow::anyhow!("failed to parse passphrase hash: {err}"))?;
    Ok(argon2id()?
        .verify_password(passphrase.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::{
        constant_time_str_eq, hash_passphrase, normalize_optional_passphrase, verify_passphrase,
    };

    #[test]
    fn empty_passphrase_normalizes_to_none() {
        assert_eq!(normalize_optional_passphrase("   "), None);
    }

    #[test]
    fn constant_time_eq_matches_string_equality() {
        assert!(constant_time_str_eq("secret", "secret"));
        assert!(!constant_time_str_eq("secret", "secreT"));
        assert!(!constant_time_str_eq("secret", "secret-longer"));
        assert!(!constant_time_str_eq("secret", ""));
        assert!(constant_time_str_eq("", ""));
    }

    #[test]
    fn argon2_hash_round_trip_verifies() {
        let hash = hash_passphrase("super-segura").expect("hash passphrase");
        assert!(verify_passphrase(&hash, "super-segura").expect("verify passphrase"));
        assert!(!verify_passphrase(&hash, "otra-cosa").expect("verify wrong passphrase"));
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn security_phc_parameters_are_rejected_before_expensive_verification() {
        let valid = hash_passphrase("fixture-passphrase").unwrap();
        validate_passphrase_hash(&valid).unwrap();
        for invalid in [
            valid.replace("m=19456", "m=4294967295"),
            valid.replace("m=19456", "m=8"),
            valid.replace("t=2", "t=4294967295"),
            valid.replace("t=2", "t=1"),
            valid.replace("p=1", "p=2"),
            valid.replace("argon2id", "argon2i"),
            valid.replace("v=19", "v=16"),
            "x".repeat(MAX_PASSPHRASE_HASH_BYTES + 1),
        ] {
            assert!(validate_passphrase_hash(&invalid).is_err());
            assert!(verify_passphrase(&invalid, "fixture-passphrase").is_err());
        }
        assert!(hash_passphrase(&"x".repeat(MAX_PASSPHRASE_BYTES + 1)).is_err());
        assert!(verify_passphrase(&valid, &"x".repeat(MAX_PASSPHRASE_BYTES + 1)).is_err());
    }
}
