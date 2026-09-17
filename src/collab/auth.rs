use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;
pub const MIN_PASSPHRASE_LEN: usize = 8;

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

pub fn verify_passphrase(hash: &str, passphrase: &str) -> anyhow::Result<bool> {
    validate_passphrase_hash(hash)?;
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

/// Validate before invoking Argon2: PHC parameters override verifier defaults.
pub fn validate_passphrase_hash(hash: &str) -> anyhow::Result<()> {
    anyhow::ensure!(hash.len() <= 256, "Passphrase hash too large");
    let parsed = PasswordHash::new(hash).map_err(|_| anyhow::anyhow!("Invalid passphrase hash"))?;
    let params =
        Params::try_from(&parsed).map_err(|_| anyhow::anyhow!("Invalid Argon2 parameters"))?;
    anyhow::ensure!(
        parsed.algorithm.as_str() == "argon2id"
            && parsed.version == Some(19)
            && params.m_cost() == ARGON2_MEMORY_KIB
            && params.t_cost() == ARGON2_ITERATIONS
            && params.p_cost() == ARGON2_PARALLELISM
            && parsed.params.iter().count() == 3,
        "Unsupported passphrase hash policy"
    );
    let mut salt_bytes = [0u8; 64];
    let salt = parsed.salt.ok_or_else(|| anyhow::anyhow!("Missing salt"))?;
    let decoded = salt
        .decode_b64(&mut salt_bytes)
        .map_err(|_| anyhow::anyhow!("Invalid salt"))?;
    anyhow::ensure!(
        decoded.len() == 16 && parsed.hash.is_some_and(|output| output.len() == 32),
        "Invalid passphrase hash size"
    );
    Ok(())
}
#[cfg(test)]
mod security_policy_tests {
    use super::*;
    #[test]
    fn only_the_generated_bounded_hash_policy_is_accepted() {
        let hash = hash_passphrase("dummy-password").unwrap();
        validate_passphrase_hash(&hash).unwrap();
        for altered in [
            hash.replace("m=19456", "m=19457"),
            hash.replace("t=2", "t=3"),
            hash.replace("p=1", "p=2"),
            hash.replace("argon2id", "argon2i"),
            hash.replace("v=19", "v=16"),
        ] {
            assert!(validate_passphrase_hash(&altered).is_err());
            assert!(verify_passphrase(&altered, "dummy-password").is_err());
        }
        for invalid in ["", "not-a-hash", "$argon2id$v=19$m=19456,t=2,p=1"] {
            assert!(validate_passphrase_hash(invalid).is_err());
        }
    }
}
