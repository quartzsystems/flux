//! TLS certificate handling.
//!
//! An appliance on a management network is still worth encrypting: the session
//! cookie it issues is a bearer credential, and the API it fronts starts
//! line-rate traffic. This module owns parsing, validating, and storing the
//! material; `main` decides whether to bind a TLS listener with it.
//!
//! Material is validated *before* anything is written. An appliance that
//! accepted a broken certificate and then failed to bind its listener would be
//! unreachable, which is the one failure this path must not produce.

use std::path::{Path, PathBuf};

/// Filename the certificate chain is stored under.
const CERT_FILE: &str = "server.crt";

/// Filename the private key is stored under.
const KEY_FILE: &str = "server.key";

/// Permissions for the private key.
///
/// Owner read/write only. The key is the one file on the appliance whose
/// disclosure undoes everything else this module is for.
#[cfg(unix)]
const KEY_MODE: u32 = 0o600;

/// Why some material could not be used.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The certificate could not be parsed.
    #[error("the certificate is not valid PEM: {0}")]
    BadCertificate(String),

    /// The private key could not be parsed.
    #[error("the private key is not valid PEM: {0}")]
    BadKey(String),

    /// Reading or writing failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Where the installed material lives.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The certificate chain.
    pub certificate: PathBuf,
    /// The private key.
    pub private_key: PathBuf,
}

impl Paths {
    /// The paths under `dir`.
    pub fn in_dir(dir: &Path) -> Self {
        Self { certificate: dir.join(CERT_FILE), private_key: dir.join(KEY_FILE) }
    }

    /// True when both files are present.
    pub fn present(&self) -> bool {
        self.certificate.is_file() && self.private_key.is_file()
    }
}

/// What a certificate says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Material {
    /// A description of the leaf certificate, for the settings page.
    pub subject: String,
    /// When it expires, as an RFC 3339 timestamp, if that could be read.
    pub not_after: Option<String>,
}

impl Material {
    /// Parses and cross-checks a certificate and key.
    ///
    /// The parsing here is deliberately structural: it confirms the PEM blocks
    /// are present, well-formed, and of the right kinds. It does not verify a
    /// signature chain, because an appliance certificate is routinely
    /// self-signed or issued by an internal authority this appliance has no
    /// reason to trust a priori.
    pub fn parse(certificate: &str, private_key: &str) -> Result<Self, TlsError> {
        // Checked first: material pasted into the wrong box is the common
        // mistake, and the generic "no such block" message below would be a
        // much less useful thing to tell an operator who has done it.
        if certificate.contains("PRIVATE KEY") {
            return Err(TlsError::BadCertificate(
                "this looks like a private key, not a certificate".into(),
            ));
        }
        if private_key.contains("BEGIN CERTIFICATE") {
            return Err(TlsError::BadKey(
                "this looks like a certificate, not a private key".into(),
            ));
        }

        let certs = pem_blocks(certificate);
        let leaf = certs
            .iter()
            .find(|(label, _)| label == "CERTIFICATE")
            .ok_or_else(|| TlsError::BadCertificate("no CERTIFICATE block found".into()))?;

        if leaf.1.is_empty() {
            return Err(TlsError::BadCertificate("the CERTIFICATE block is empty".into()));
        }

        let keys = pem_blocks(private_key);
        let key = keys
            .iter()
            .find(|(label, _)| label.ends_with("PRIVATE KEY"))
            .ok_or_else(|| TlsError::BadKey("no PRIVATE KEY block found".into()))?;

        if key.1.is_empty() {
            return Err(TlsError::BadKey("the PRIVATE KEY block is empty".into()));
        }

        // TODO(tls-verify): cross-checking that the key actually belongs to
        // the certificate needs an X.509 parser, which is not linked. Until it
        // is, a mismatched pair is caught when the listener refuses to bind at
        // startup rather than at upload time.

        Ok(Self {
            // TODO(tls-verify): the subject and expiry come from the DER, which
            // needs an X.509 parser to read. Until one is linked, the settings
            // page shows the chain length rather than claiming a subject it has
            // not actually read.
            subject: format!(
                "{} certificate{} uploaded",
                certs.len(),
                if certs.len() == 1 { "" } else { "s" }
            ),
            not_after: None,
        })
    }
}

/// Splits a PEM document into its labelled blocks.
///
/// Returns the label and the base64 body of each, with no attempt to decode:
/// this is a structural check, and a decoder here would be a second parser to
/// keep correct.
fn pem_blocks(pem: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut label: Option<String> = None;
    let mut body = String::new();

    for line in pem.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            label = rest.strip_suffix("-----").map(str::to_string);
            body.clear();
        } else if line.starts_with("-----END ") {
            if let Some(name) = label.take() {
                blocks.push((name, std::mem::take(&mut body)));
            }
        } else if label.is_some() && !line.is_empty() {
            body.push_str(line);
        }
    }

    blocks
}

/// Writes validated material to disk.
///
/// The key is written with restrictive permissions before its contents, so it
/// is never briefly world-readable.
pub fn install(dir: &Path, certificate: &str, private_key: &str) -> Result<Paths, TlsError> {
    std::fs::create_dir_all(dir)?;
    let paths = Paths::in_dir(dir);

    std::fs::write(&paths.certificate, certificate)?;
    write_private(&paths.private_key, private_key)?;

    tracing::info!(dir = %dir.display(), "installed TLS material");
    Ok(paths)
}

/// Writes the private key with owner-only permissions.
#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> Result<(), TlsError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(KEY_MODE)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// Writes the private key.
///
/// Windows has no mode bits to set here; the appliance is Linux, and this path
/// exists so the daemon builds on a developer machine.
#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> Result<(), TlsError> {
    std::fs::write(path, contents)?;
    Ok(())
}

/// Removes installed material.
///
/// A missing file is the outcome we wanted, not an error.
pub fn remove(dir: &Path) -> Result<(), TlsError> {
    let paths = Paths::in_dir(dir);

    for path in [&paths.certificate, &paths.private_key] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A structurally valid certificate block.
    const CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAKZ\nZm9vYmFy\n-----END CERTIFICATE-----\n";

    /// A structurally valid key block.
    const KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBg\na2V5\n-----END PRIVATE KEY-----\n";

    #[test]
    fn well_formed_material_parses() {
        let material = Material::parse(CERT, KEY).unwrap();
        assert!(material.subject.contains("1 certificate"));
    }

    #[test]
    fn a_chain_reports_every_certificate_in_it() {
        let chain = format!("{CERT}{CERT}");
        assert!(Material::parse(&chain, KEY).unwrap().subject.contains("2 certificates"));
    }

    #[test]
    fn an_rsa_key_block_is_accepted_as_well_as_pkcs8() {
        let rsa = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\n";
        assert!(Material::parse(CERT, rsa).is_ok());
    }

    #[test]
    fn material_pasted_into_the_wrong_box_is_diagnosed() {
        // The common mistake, and one that otherwise fails much later with a
        // message about the listener rather than the upload.
        match Material::parse(KEY, KEY) {
            Err(TlsError::BadCertificate(message)) => {
                assert!(message.contains("private key"), "{message}");
            }
            other => panic!("expected a certificate diagnosis, got {other:?}"),
        }

        match Material::parse(CERT, CERT) {
            Err(TlsError::BadKey(message)) => {
                assert!(message.contains("certificate"), "{message}");
            }
            other => panic!("expected a key diagnosis, got {other:?}"),
        }
    }

    #[test]
    fn material_that_is_not_pem_is_rejected() {
        assert!(Material::parse("not a certificate", KEY).is_err());
        assert!(Material::parse(CERT, "not a key").is_err());
        assert!(Material::parse("", "").is_err());
    }

    #[test]
    fn an_empty_block_is_rejected_rather_than_installed() {
        let empty = "-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n";
        assert!(Material::parse(empty, KEY).is_err());
    }

    #[test]
    fn pem_blocks_are_split_by_label() {
        let blocks = pem_blocks(&format!("{CERT}{KEY}"));
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "CERTIFICATE");
        assert_eq!(blocks[1].0, "PRIVATE KEY");
        assert!(!blocks[0].1.is_empty());
    }

    #[test]
    fn text_around_the_blocks_is_ignored() {
        // Certificate authorities routinely wrap PEM in explanatory text.
        let noisy = format!("Issued by Example CA\n\n{CERT}\nThank you.\n");
        assert_eq!(pem_blocks(&noisy).len(), 1);
    }

    #[test]
    fn installing_then_removing_leaves_no_material_behind() {
        let dir = std::env::temp_dir().join(format!("flux-tls-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let paths = install(&dir, CERT, KEY).unwrap();
        assert!(paths.present());
        assert_eq!(std::fs::read_to_string(&paths.certificate).unwrap(), CERT);

        remove(&dir).unwrap();
        assert!(!paths.present());

        // Removing again is the outcome we wanted, not an error.
        remove(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_are_reported_under_the_configured_directory() {
        let paths = Paths::in_dir(Path::new("/etc/flux/tls"));
        assert!(paths.certificate.ends_with("server.crt"));
        assert!(paths.private_key.ends_with("server.key"));
        assert!(!paths.present(), "nothing is installed at that path in a test");
    }
}
