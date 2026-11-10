//! OpenSSH host certificate policy: scoped, explicitly trusted CAs; no TOFU fallback.

use std::path::Path;

use anyhow::{Context as _, bail};
use hmac::{Hmac, KeyInit as _, Mac as _};
use playterm_i18n::t;
use russh::keys::ssh_key::certificate::CertType;
use russh::keys::ssh_key::known_hosts::{HostPatterns, KnownHosts, Marker};
use russh::keys::{Certificate, HashAlg, PublicKey};
use sha1::Sha1;

#[derive(Default)]
pub(crate) struct Authorities {
    trusted: Vec<PublicKey>,
    revoked: Vec<PublicKey>,
}

impl Authorities {
    pub fn configured(&self) -> bool {
        !self.trusted.is_empty()
    }

    pub fn is_revoked(&self, key: &PublicKey) -> bool {
        self.revoked.iter().any(|entry| entry.key_data() == key.key_data())
    }

    pub fn verify(&self, cert: &Certificate, host: &str, now: u64) -> anyhow::Result<()> {
        if !self.configured() {
            bail!(t!("host_certificate.no_trusted_ssh_ca_is_configured_for_this"));
        }
        if cert.cert_type() != CertType::Host {
            bail!(t!("host_certificate.this_is_not_an_ssh_host_certificate"));
        }
        if !cert.valid_principals().iter().any(|principal| wildcard_match(principal, host)) {
            bail!(t!("host_certificate.the_ssh_certificate_hostname_does_not_match"));
        }
        // No critical options are defined for OpenSSH host certificates. Unknown ones
        // must fail closed; noncritical extensions may be ignored.
        if !cert.critical_options().is_empty() {
            bail!(t!("host_certificate.unsupported_ssh_certificate_critical_options"));
        }
        if self.revoked.iter().any(|key| key.key_data() == cert.signature_key() || key.key_data() == cert.public_key()) {
            bail!(t!("host_certificate.the_ssh_certificate_key_or_ca_is_revoked"));
        }
        let fingerprints: Vec<_> = self.trusted.iter().map(|key| key.fingerprint(HashAlg::Sha256)).collect();
        cert.validate_at(now, &fingerprints).context(t!("host_certificate.ssh_certificate_signature_ca_or_validity_check_failed"))
    }
}

pub(crate) fn load(path: &Path, host: &str, port: u16) -> anyhow::Result<Authorities> {
    parse(&crate::trust::read(path)?, host, port)
}

fn parse(text: &str, host: &str, port: u16) -> anyhow::Result<Authorities> {
    let target = if port == 22 { host.to_owned() } else { format!("[{host}]:{port}") };
    let mut authorities = Authorities::default();
    for raw in text.lines().map(str::trim).filter(|line| line.starts_with('@')) {
        let entry = KnownHosts::new(raw).next().context("Empty SSH trust marker")??;
        if !matches_host(entry.host_patterns(), &target) {
            continue;
        }
        match entry.marker() {
            Some(Marker::CertAuthority) => authorities.trusted.push(entry.public_key().clone()),
            Some(Marker::Revoked) => authorities.revoked.push(entry.public_key().clone()),
            None => {}
        }
    }
    Ok(authorities)
}

fn matches_host(patterns: &HostPatterns, host: &str) -> bool {
    match patterns {
        HostPatterns::HashedName { salt, hash } => {
            Hmac::<Sha1>::new_from_slice(salt).is_ok_and(|mac| mac.chain_update(host.as_bytes()).verify_slice(hash).is_ok())
        }
        HostPatterns::Patterns(patterns) => {
            let mut positive = false;
            for pattern in patterns {
                let (negated, pattern) = pattern.strip_prefix('!').map_or((false, pattern.as_str()), |p| (true, p));
                if wildcard_match(pattern, host) {
                    if negated {
                        return false;
                    }
                    positive = true;
                }
            }
            positive
        }
    }
}

/// OpenSSH host patterns use only '*' and '?' as wildcards; brackets are literal IPv6/port syntax.
fn wildcard_match(pattern: &str, host: &str) -> bool {
    let (pattern, host) = (pattern.as_bytes(), host.as_bytes());
    let (mut p, mut h, mut star, mut retry) = (0, 0, None, 0);
    while h < host.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p].eq_ignore_ascii_case(&host[h])) {
            p += 1;
            h += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = h;
        } else if let Some(at) = star {
            retry += 1;
            h = retry;
            p = at + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::certificate::Builder;
    use russh::keys::{Algorithm, PrivateKey};

    fn key() -> PrivateKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
    }

    fn certificate(ca: &PrivateKey, principal: &str, after: u64, before: u64, kind: CertType, critical: bool) -> Certificate {
        let host = key();
        let mut builder = Builder::new_with_random_nonce(&mut rand::rng(), host.public_key(), after, before).unwrap();
        builder.cert_type(kind).unwrap();
        if !principal.is_empty() {
            builder.valid_principal(principal).unwrap();
        } else {
            builder.all_principals_valid().unwrap();
        }
        if critical {
            builder.critical_option("unknown-critical", "").unwrap();
        }
        builder.sign(ca).unwrap()
    }

    #[test]
    fn validates_ca_signature_time_host_type_principal_and_critical_options() {
        let ca = key();
        let trust = parse(&format!("@cert-authority *.example {}", ca.public_key().to_openssh().unwrap()), "db.example", 22).unwrap();
        let valid = certificate(&ca, "db.example", 10, 20, CertType::Host, false);
        trust.verify(&valid, "db.example", 15).unwrap();
        assert!(trust.verify(&valid, "other.example", 15).is_err());
        assert!(trust.verify(&valid, "db.example", 9).is_err());
        assert!(trust.verify(&valid, "db.example", 20).is_err());
        assert!(trust.verify(&certificate(&key(), "db.example", 10, 20, CertType::Host, false), "db.example", 15).is_err());
        assert!(trust.verify(&certificate(&ca, "db.example", 10, 20, CertType::User, false), "db.example", 15).is_err());
        assert!(trust.verify(&certificate(&ca, "", 10, 20, CertType::Host, false), "db.example", 15).is_err());
        assert!(trust.verify(&certificate(&ca, "db.example", 10, 20, CertType::Host, true), "db.example", 15).is_err());
        assert!(Authorities::default().verify(&valid, "db.example", 15).is_err());
        let mut damaged = valid.to_bytes().unwrap();
        *damaged.last_mut().unwrap() ^= 1;
        assert!(trust.verify(&Certificate::from_bytes(&damaged).unwrap(), "db.example", 15).is_err());
        let revoked = parse(
            &format!(
                "@cert-authority *.example {}\n@revoked *.example {}",
                ca.public_key().to_openssh().unwrap(),
                ca.public_key().to_openssh().unwrap()
            ),
            "db.example",
            22,
        )
        .unwrap();
        assert!(revoked.verify(&valid, "db.example", 15).is_err());
    }

    #[test]
    fn wildcard_principals_remain_scoped_and_require_a_trusted_ca() {
        let ca = key();
        let trust = parse(&format!("@cert-authority *.example {}", ca.public_key().to_openssh().unwrap()), "db.example", 22).unwrap();
        for principal in ["*.example", "d?.example", "DB.EXAMPLE"] {
            let cert = certificate(&ca, principal, 10, 20, CertType::Host, false);
            trust.verify(&cert, "db.example", 15).unwrap();
            assert!(trust.verify(&cert, "db.example.attacker", 15).is_err());
            assert!(trust.verify(&cert, "example", 15).is_err());
            assert!(Authorities::default().verify(&cert, "db.example", 15).is_err());
        }
        let wrong_ca = certificate(&key(), "*.example", 10, 20, CertType::Host, false);
        assert!(trust.verify(&wrong_ca, "db.example", 15).is_err());
    }

    #[test]
    fn authority_patterns_are_scoped_by_host_negation_and_port() {
        let ca = key().public_key().to_openssh().unwrap();
        let text = format!("@cert-authority *.example,!blocked.example {ca}\n@cert-authority [special.example]:2222 {ca}");
        assert!(parse(&text, "DB.example", 22).unwrap().configured());
        assert!(!parse(&text, "blocked.example", 22).unwrap().configured());
        assert!(!parse(&text, "db.example.attacker", 22).unwrap().configured());
        assert!(!parse(&text, "db.example", 2222).unwrap().configured());
        assert!(parse(&text, "special.example", 2222).unwrap().configured());
        assert!(!parse(&text, "special.example", 2223).unwrap().configured());
        assert!(wildcard_match("a*?z", "abcdez"));
        assert!(!wildcard_match("a*?z", "az"));
        use base64::Engine as _;
        let salt = b"test-only-salt";
        let hash = Hmac::<Sha1>::new_from_slice(salt).unwrap().chain_update(b"[db.example]:2222").finalize().into_bytes();
        let b64 = base64::engine::general_purpose::STANDARD;
        let hashed = format!("@cert-authority |1|{}|{} {ca}", b64.encode(salt), b64.encode(hash));
        assert!(parse(&hashed, "db.example", 2222).unwrap().configured());
        assert!(!parse(&hashed, "db.example", 22).unwrap().configured());
    }
}
