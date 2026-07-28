//! Password hashing, session tokens, and the authenticated principal.
//!
//! Two secrets exist in this system and they are handled differently:
//!
//! * **Passwords** are hashed with Argon2id and never stored or logged in any
//!   other form. Verification is constant-time by construction.
//! * **Session tokens** are 256 bits of OS randomness. The plaintext goes into an
//!   httpOnly cookie; only its SHA-256 is written to the database. A token is
//!   already high-entropy, so a fast hash is the right choice here — Argon2 would
//!   add latency to every single request and defend against nothing, since there
//!   is no low-entropy input to brute-force.

use std::sync::LazyLock;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use flux_core::types::{Id, Role};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "flux_session";

/// Shortest password we will accept.
///
/// The appliance is usually on a management VLAN rather than the open internet,
/// but it controls test equipment and holds results, so this is not a throwaway
/// login. Twelve characters with no composition rules is the current NIST-aligned
/// advice and is easier to satisfy with a passphrase than eight with symbols.
pub const MIN_PASSWORD_LEN: usize = 12;

/// Longest password we will hash.
///
/// Argon2 handles arbitrary lengths, but hashing an unbounded body on an
/// unauthenticated endpoint is a free denial-of-service primitive.
pub const MAX_PASSWORD_LEN: usize = 256;

/// A hash string that no password can match.
///
/// Used to spend the same Argon2 time on a login for a username that does not
/// exist as on one that does. Without it, response latency reveals which
/// usernames are real.
///
/// It is derived at first use from a fresh random secret rather than hard-coded,
/// so it is guaranteed to be a well-formed PHC string with the current Argon2
/// parameters — a hard-coded constant would silently stop costing the right
/// amount of time the moment those parameters were tuned.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    let secret = generate_token();
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .expect("hashing a fixed-length hex string with default parameters cannot fail")
        .to_string()
});

/// The authenticated principal behind a request.
///
/// Built once by the authentication middleware and carried in the request
/// extensions, so handlers never repeat the session lookup.
#[derive(Debug, Clone)]
pub struct Identity {
    /// Session primary key, for audit logging and logout.
    pub session_id: Id,
    /// Authenticated account.
    pub user_id: Id,
    /// Account login name.
    pub username: String,
    /// Account access level.
    pub role: Role,
}

impl Identity {
    /// True when this principal meets `required`.
    pub fn can(&self, required: Role) -> bool {
        self.role.has_at_least(required)
    }
}

/// Reasons a password will not be accepted.
#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    /// Shorter than [`MIN_PASSWORD_LEN`].
    #[error("password must be at least {MIN_PASSWORD_LEN} characters")]
    TooShort,
    /// Longer than [`MAX_PASSWORD_LEN`].
    #[error("password must be at most {MAX_PASSWORD_LEN} characters")]
    TooLong,
    /// The hashing primitive itself failed, which should not happen.
    #[error("could not hash password: {0}")]
    Hash(String),
}

/// Checks a candidate password against the length policy.
pub fn check_password_policy(plain: &str) -> Result<(), PasswordError> {
    if plain.chars().count() < MIN_PASSWORD_LEN {
        return Err(PasswordError::TooShort);
    }
    if plain.len() > MAX_PASSWORD_LEN {
        return Err(PasswordError::TooLong);
    }
    Ok(())
}

/// Hashes a password with Argon2id and a fresh random salt.
///
/// Returns a PHC string, which embeds the algorithm, parameters, and salt — so a
/// future parameter change can re-hash on next login without a migration.
pub fn hash_password(plain: &str) -> Result<String, PasswordError> {
    check_password_policy(plain)?;
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| PasswordError::Hash(e.to_string()))
}

/// Verifies a password against a stored PHC string.
///
/// Returns `false` for a malformed stored hash rather than erroring: a corrupt
/// row must fail the login, not take the endpoint down.
pub fn verify_password(plain: &str, phc: &str) -> bool {
    // Refuse before hashing so an oversized body cannot burn CPU.
    if plain.len() > MAX_PASSWORD_LEN {
        return false;
    }
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default().verify_password(plain.as_bytes(), &parsed).is_ok(),
        Err(err) => {
            tracing::error!(%err, "stored password hash is malformed");
            false
        }
    }
}

/// Spends roughly one verification's worth of time against a fixed hash.
///
/// Called on the "no such user" path so that login latency does not distinguish
/// a real username from an invented one.
pub fn verify_dummy(plain: &str) {
    let _ = verify_password(plain, &DUMMY_HASH);
}

/// Generates a session token: 32 bytes of OS randomness, hex encoded.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    // `OsRng` reads the platform CSPRNG and panics only if the OS cannot supply
    // entropy, which is not a condition we can meaningfully continue from.
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Hashes a session token for storage.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_verifies_against_its_own_hash_and_nothing_else() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("correct horse batter", &hash));
        assert!(!verify_password("", &hash));
    }

    #[test]
    fn the_same_password_hashes_differently_every_time() {
        // A fresh salt per hash is what stops a stolen table from revealing which
        // accounts share a password.
        let a = hash_password("correct horse battery").unwrap();
        let b = hash_password("correct horse battery").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("correct horse battery", &a));
        assert!(verify_password("correct horse battery", &b));
    }

    #[test]
    fn password_policy_bounds_are_enforced_at_hash_time() {
        assert!(matches!(hash_password("short"), Err(PasswordError::TooShort)));
        assert!(matches!(
            hash_password(&"x".repeat(MAX_PASSWORD_LEN + 1)),
            Err(PasswordError::TooLong)
        ));
        assert!(hash_password(&"x".repeat(MIN_PASSWORD_LEN)).is_ok());
    }

    #[test]
    fn policy_length_is_counted_in_characters_not_bytes() {
        // Twelve multi-byte characters is a twelve-character password.
        let passphrase = "\u{00e9}".repeat(MIN_PASSWORD_LEN);
        assert!(passphrase.len() > MIN_PASSWORD_LEN, "test needs a multi-byte string");
        assert!(check_password_policy(&passphrase).is_ok());
    }

    #[test]
    fn a_malformed_stored_hash_fails_the_login_instead_of_panicking() {
        assert!(!verify_password("anything", "not-a-phc-string"));
        assert!(!verify_password("anything", ""));
    }

    #[test]
    fn the_dummy_hash_is_well_formed_so_the_timing_defence_actually_runs() {
        // If this were malformed, `verify_password` would bail early on a parse
        // error and the equal-time property would silently disappear.
        assert!(PasswordHash::new(&DUMMY_HASH).is_ok());
        assert!(!verify_password("anything", &DUMMY_HASH));
        verify_dummy("anything");
    }

    #[test]
    fn session_tokens_are_unique_and_full_entropy() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64, "32 bytes hex encoded");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn token_hashing_is_deterministic_and_hides_the_token() {
        let token = generate_token();
        assert_eq!(hash_token(&token), hash_token(&token));
        assert_ne!(hash_token(&token), token);
        assert_eq!(hash_token(&token).len(), 64);
    }

    #[test]
    fn identity_role_checks_follow_the_privilege_order() {
        let op = Identity {
            session_id: Id::nil(),
            user_id: Id::nil(),
            username: "op".into(),
            role: Role::Operator,
        };
        assert!(op.can(Role::Viewer));
        assert!(op.can(Role::Operator));
        assert!(!op.can(Role::Admin));
    }
}
