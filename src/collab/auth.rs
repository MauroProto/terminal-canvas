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

pub fn verify_passphrase(hash: &str, passphrase: &str) -> anyhow::Result<bool> {
    let parsed = PasswordHash::new(hash)
        .map_err(|err| anyhow::anyhow!("failed to parse passphrase hash: {err}"))?;
    Ok(argon2id()?
        .verify_password(passphrase.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::{hash_passphrase, normalize_optional_passphrase, verify_passphrase};

    #[test]
    fn empty_passphrase_normalizes_to_none() {
        assert_eq!(normalize_optional_passphrase("   "), None);
    }

    #[test]
    fn argon2_hash_round_trip_verifies() {
        let hash = hash_passphrase("super-segura").expect("hash passphrase");
        assert!(verify_passphrase(&hash, "super-segura").expect("verify passphrase"));
        assert!(!verify_passphrase(&hash, "otra-cosa").expect("verify wrong passphrase"));
    }
}
