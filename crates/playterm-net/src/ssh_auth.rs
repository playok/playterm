//! SSH user authentication, tried in roughly the order OpenSSH uses:
//!
//! 1. `none` (learns which methods the server accepts)
//! 2. the explicitly configured private key file
//! 3. keys held by an SSH agent (Windows: OpenSSH agent pipe, then Pageant; Unix: `SSH_AUTH_SOCK`)
//! 4. default key files (`~/.ssh/id_ed25519`, ...)
//! 5. the stored password
//! 6. keyboard-interactive / password prompts

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::bail;
use base64::Engine as _;
use playterm_i18n::{t, tr};
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{self, Algorithm, Certificate, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::{MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite};
use zeroize::Zeroizing;

use crate::session::{ConnectParams, EventSink, PromptField, PromptResponse};
use crate::ssh::ClientHandler;

type Session = client::Handle<ClientHandler>;

struct AuthState {
    methods: MethodSet,
    second_factor: bool,
    attempted_keys: HashSet<String>,
    accepted_keys: HashSet<String>,
}

impl AuthState {
    fn allows_keys(&self) -> bool {
        allows(&self.methods, MethodKind::PublicKey)
    }

    /// One completed attempt per public key across files and every agent, per connection.
    fn claim_key(&mut self, key: &PublicKey) -> bool {
        let id = key.fingerprint(HashAlg::Sha256).to_string();
        !self.accepted_keys.contains(&id) && self.attempted_keys.insert(id)
    }

    fn claim_certificate(&mut self, cert: &Certificate) -> anyhow::Result<bool> {
        let key = PublicKey::new(cert.public_key().clone(), "");
        Ok(!self.accepted_keys.contains(&key.fingerprint(HashAlg::Sha256).to_string()) && self.attempted_keys.insert(certificate_id(cert)?))
    }

    fn record_key(&mut self, result: &AuthResult, key: &PublicKey) {
        if matches!(result, AuthResult::Failure { partial_success: true, .. }) {
            self.accepted_keys.insert(key.fingerprint(HashAlg::Sha256).to_string());
        }
    }
}

const MAX_AUTH_ROUNDS: usize = 3;
const MAX_PASSPHRASE_TRIES: usize = 3;

fn certificate_id(cert: &Certificate) -> anyhow::Result<String> {
    // Comments are not part of the identity; a file and an agent may describe it differently.
    Ok(format!("certificate:{}", base64::engine::general_purpose::STANDARD.encode(cert.to_bytes()?)))
}

/// Pipe served by the Windows OpenSSH `ssh-agent` service.
#[cfg(windows)]
const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

fn allows(methods: &MethodSet, kind: MethodKind) -> bool {
    methods.contains(&kind)
}

pub(crate) async fn authenticate(session: &mut Session, params: &ConnectParams, sink: &EventSink) -> anyhow::Result<()> {
    let user = params.username.as_str();
    if user.is_empty() {
        bail!(t!("ssh_auth.an_ssh_user_name_is_required"));
    }

    let mut state = match session.authenticate_none(user).await? {
        AuthResult::Success => return Ok(()),
        AuthResult::Failure { remaining_methods, partial_success } => AuthState {
            methods: remaining_methods,
            second_factor: partial_success,
            attempted_keys: HashSet::new(),
            accepted_keys: HashSet::new(),
        },
    };

    if allows(&state.methods, MethodKind::PublicKey) && public_keys(session, params, sink, &mut state).await? {
        return Ok(());
    }
    passwords(session, params, sink, state).await
}

// -------------------------------------------------------------------------------------------------
// Public keys

/// Outcome of one public-key attempt.
enum KeyAttempt {
    Accepted,
    /// Rejected; `false` once the server no longer offers public-key auth.
    Rejected {
        keep_trying: bool,
    },
    /// The key could not be used (missing file, explicit skip, agent refused).
    Unusable,
}

async fn public_keys(session: &mut Session, params: &ConnectParams, sink: &EventSink, state: &mut AuthState) -> anyhow::Result<bool> {
    let certificate = match &params.certificate_file {
        Some(path) => Some(read_certificate(path).await?),
        None => None,
    };
    if let Some(path) = &params.identity_file {
        match key_file(session, params, sink, path, true, certificate.as_ref(), state).await? {
            KeyAttempt::Accepted => return Ok(true),
            KeyAttempt::Rejected { keep_trying: false } => return Ok(false),
            _ => {}
        }
    }

    if params.use_agent && agents(session, params, certificate.as_ref(), state).await? {
        return Ok(true);
    }
    if !state.allows_keys() {
        return Ok(false);
    }

    for path in &params.default_identity_files {
        match key_file(session, params, sink, path, false, certificate.as_ref(), state).await? {
            KeyAttempt::Accepted => return Ok(true),
            KeyAttempt::Rejected { keep_trying: false } => return Ok(false),
            _ => {}
        }
    }
    Ok(false)
}

/// Hash for RSA signatures: the best the server advertises, SHA-256 if it says nothing.
async fn rsa_hash(session: &Session, algorithm: Algorithm) -> anyhow::Result<Option<HashAlg>> {
    if !algorithm.is_rsa() {
        return Ok(None);
    }
    Ok(session.best_supported_rsa_hash().await?.unwrap_or(Some(HashAlg::Sha256)))
}

fn record(result: AuthResult, state: &mut AuthState) -> KeyAttempt {
    match result {
        AuthResult::Success => KeyAttempt::Accepted,
        AuthResult::Failure { remaining_methods, partial_success } => {
            state.methods = remaining_methods;
            state.second_factor |= partial_success;
            KeyAttempt::Rejected { keep_trying: state.allows_keys() }
        }
    }
}

async fn read_certificate(path: &Path) -> anyhow::Result<Certificate> {
    use anyhow::Context as _;
    let text =
        tokio::fs::read_to_string(path).await.with_context(|| tr!("ssh_auth.cannot_read_user_certificate", arg0 = path.display()))?;
    let cert = Certificate::from_openssh(&text).with_context(|| tr!("ssh_auth.cannot_parse_user_certificate", arg0 = path.display()))?;
    if cert.cert_type() != keys::ssh_key::certificate::CertType::User {
        bail!(t!("ssh_auth.this_is_not_an_ssh_user_certificate"));
    }
    Ok(cert)
}

#[allow(clippy::too_many_arguments)]
async fn key_file(
    session: &mut Session,
    params: &ConnectParams,
    sink: &EventSink,
    path: &Path,
    explicit: bool,
    certificate: Option<&Certificate>,
    state: &mut AuthState,
) -> anyhow::Result<KeyAttempt> {
    let read = async {
        let mut file = tokio::fs::File::open(path).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if file.metadata().await?.permissions().mode() & 0o077 != 0 {
                sink.notice(tr!("ssh_auth.other_users_may_read_or_write_the_private", arg0 = path.display()));
            }
        }
        let mut text = Zeroizing::new(String::new());
        file.read_to_string(&mut text).await?;
        Ok::<_, std::io::Error>(text)
    };
    let text = match read.await {
        Ok(text) => text,
        Err(e) => {
            if explicit {
                sink.notice(tr!("ssh_auth.cannot_read_the_key_file", arg0 = path.display(), e = e));
            }
            return Ok(KeyAttempt::Unusable);
        }
    };
    let Some(key) = decode_key(&text, path, sink).await? else {
        return Ok(KeyAttempt::Unusable);
    };
    // RustCrypto rsa currently has no RustSec-patched version for RUSTSEC-2023-0071.
    // Apply this to the decoded algorithm, before claiming/offering/signing the key,
    // so renamed files and every default/partial-success/jump fallback are covered.
    // Agent keys follow a separate external-signer path and are not claimed here.
    if key.algorithm().is_rsa() {
        sink.notice(tr!("ssh_auth.local_rsa_private_key_signing_is_blocked_because", arg0 = path.display()));
        return Ok(KeyAttempt::Unusable);
    }
    let discovered;
    let certificate = if certificate.is_some() {
        certificate
    } else {
        let mut cert_path = path.as_os_str().to_owned();
        cert_path.push("-cert.pub");
        discovered = match read_certificate(Path::new(&cert_path)).await {
            Ok(cert) => Some(cert),
            Err(e) => {
                if tokio::fs::try_exists(&cert_path).await.unwrap_or(false) {
                    sink.notice(format!("{e:#}"));
                }
                None
            }
        };
        discovered.as_ref()
    };
    if let Some(cert) = certificate {
        if cert.public_key() != key.public_key().key_data() {
            sink.notice(tr!("ssh_auth.user_certificate_does_not_match_private_key", arg0 = path.display()));
            if params.certificate_file.is_some() {
                return Ok(KeyAttempt::Unusable);
            }
        } else if state.claim_certificate(cert)? {
            let result = session.authenticate_openssh_cert(params.username.as_str(), Arc::new(key.clone()), cert.clone()).await?;
            state.record_key(&result, key.public_key());
            let attempt = record(result, state);
            if matches!(attempt, KeyAttempt::Accepted | KeyAttempt::Rejected { keep_trying: false }) || params.certificate_file.is_some() {
                return Ok(attempt);
            }
        }
    }
    if params.certificate_file.is_some() || !state.claim_key(key.public_key()) {
        return Ok(KeyAttempt::Unusable);
    }
    let hash = rsa_hash(session, key.algorithm()).await?;
    let public = key.public_key().clone();
    let result = session.authenticate_publickey(params.username.as_str(), PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?;
    state.record_key(&result, &public);
    Ok(record(result, state))
}

/// Whether a key that failed to decode without a passphrase is simply encrypted.
fn looks_encrypted(text: &str, error: &keys::Error) -> bool {
    matches!(error, keys::Error::KeyIsEncrypted)
        // PKCS#5 "Proc-Type: 4,ENCRYPTED" and PKCS#8 "BEGIN ENCRYPTED PRIVATE KEY"
        || text.contains("ENCRYPTED")
        // PuTTY .ppk: "Encryption: aes256-cbc"
        || text
            .lines()
            .find_map(|l| l.strip_prefix("Encryption:"))
            .is_some_and(|v| v.trim() != "none")
}

/// `None` skips an unusable key; cancellation aborts the entire connection.
async fn decode_key(text: &str, path: &Path, sink: &EventSink) -> anyhow::Result<Option<PrivateKey>> {
    match keys::decode_secret_key(text, None) {
        Ok(key) => return Ok(Some(key)),
        Err(e) if !looks_encrypted(text, &e) => {
            sink.notice(tr!("ssh_auth.cannot_parse_the_key_file", arg0 = path.display(), e = e));
            return Ok(None);
        }
        Err(_) => {}
    }

    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for _ in 0..MAX_PASSPHRASE_TRIES {
        let response = sink
            .prompt_response(
                tr!("ssh_auth.key_passphrase", name = name),
                path.display().to_string(),
                vec![PromptField { label: t!("ssh_auth.passphrase").into(), echo: false }],
                None,
                false,
                true,
            )
            .await;
        let answers = match response {
            PromptResponse::Answers(answers) => answers,
            PromptResponse::Skipped => return Ok(None),
            PromptResponse::Cancelled => bail!(t!("common.authentication_cancelled")),
        };
        let passphrase = Zeroizing::new(answers.into_iter().next().unwrap_or_default());
        match keys::decode_secret_key(text, Some(&passphrase)) {
            Ok(key) => return Ok(Some(key)),
            Err(_) => sink.notice(tr!("ssh_auth.wrong_passphrase", name = name)),
        }
    }
    bail!(t!("ssh_auth.too_many_key_passphrase_attempts"))
}

/// Tries every key of the reachable agents. Returns `true` on success.
async fn agents(
    session: &mut Session,
    params: &ConnectParams,
    certificate: Option<&Certificate>,
    state: &mut AuthState,
) -> anyhow::Result<bool> {
    #[cfg(windows)]
    {
        // An explicitly configured pipe (or SSH_AUTH_SOCK naming a pipe, as set by e.g.
        // 1Password or KeePassXC) replaces the default OpenSSH service pipe.
        let configured = params.agent_pipe.clone().or_else(|| std::env::var("SSH_AUTH_SOCK").ok().filter(|s| s.starts_with(r"\\.\pipe\")));
        let pipe = configured.clone().unwrap_or_else(|| OPENSSH_AGENT_PIPE.to_owned());
        match AgentClient::connect_named_pipe(&pipe).await {
            Ok(mut agent) => {
                if let Some(binding) = session.agent_session_binding() {
                    // Older agents do not know session-bind; that only disables
                    // destination-constrained keys, so the error is ignored.
                    let _ = agent.bind_session(binding, false).await;
                }
                if agent_keys(session, params, agent, certificate, state).await? {
                    return Ok(true);
                }
            }
            Err(e) => log::debug!("no SSH agent at {pipe}: {e}"),
        }

        if params.agent_pipe.is_none() && state.allows_keys() {
            match AgentClient::connect_pageant().await {
                Ok(agent) => {
                    if agent_keys(session, params, agent, certificate, state).await? {
                        return Ok(true);
                    }
                }
                Err(e) => log::debug!("Pageant not available: {e}"),
            }
        }
    }
    #[cfg(unix)]
    for socket in unix_agent_sockets(params.agent_pipe.as_deref(), std::env::var("SSH_AUTH_SOCK").ok().as_deref()) {
        if !state.allows_keys() {
            break;
        }
        match AgentClient::connect_uds(&socket).await {
            Ok(mut agent) => {
                if let Some(binding) = session.agent_session_binding() {
                    let _ = agent.bind_session(binding, false).await;
                }
                if agent_keys(session, params, agent, certificate, state).await? {
                    return Ok(true);
                }
            }
            Err(e) => log::info!("no SSH agent at {socket}: {e}"),
        }
    }
    #[cfg(not(any(windows, unix)))]
    let _ = (session, params, state);
    Ok(false)
}

/// The agent sockets to try on Unix, in order: the one configured for the site, then the one
/// named by SSH_AUTH_SOCK (the macOS launchd agent, ssh-agent, 1Password, ...). A configured
/// Windows named pipe (a site made on Windows) is no socket here and is skipped.
#[cfg(any(unix, test))]
fn unix_agent_sockets(configured: Option<&str>, env: Option<&str>) -> Vec<String> {
    let configured = configured.map(str::trim).filter(|s| !s.is_empty());
    if let Some(pipe) = configured.filter(|s| s.starts_with(r"\\")) {
        log::info!("agent pipe {pipe} is a Windows named pipe; using SSH_AUTH_SOCK instead");
    }
    let mut sockets: Vec<String> = Vec::new();
    for socket in [configured.filter(|s| !s.starts_with(r"\\")), env.map(str::trim)].into_iter().flatten() {
        if !socket.is_empty() && !sockets.iter().any(|s| s == socket) {
            sockets.push(socket.to_owned());
        }
    }
    sockets
}

async fn agent_keys<S>(
    session: &mut Session,
    params: &ConnectParams,
    mut agent: AgentClient<S>,
    certificate: Option<&Certificate>,
    state: &mut AuthState,
) -> anyhow::Result<bool>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let identities = match agent.request_identities().await {
        Ok(ids) => ids,
        Err(e) => {
            log::debug!("agent did not list identities: {e}");
            return Ok(false);
        }
    };
    // Certificates precede bare identities, even when the agent lists the bare key first.
    let mut identities = identities;
    identities.sort_by_key(|id| matches!(id, AgentIdentity::PublicKey { .. }));
    for identity in identities {
        let key = identity.public_key().into_owned();
        let comment = identity.comment().to_owned();
        let cert = certificate.or(match &identity {
            AgentIdentity::Certificate { certificate, .. } => Some(certificate),
            _ => None,
        });
        let hash = rsa_hash(session, key.algorithm()).await?;
        let (fingerprint, result) = if let Some(cert) = cert {
            if cert.cert_type() != keys::ssh_key::certificate::CertType::User
                || cert.public_key() != key.key_data()
                || !state.claim_certificate(cert)?
            {
                continue;
            }
            let mut signer = IdentitySigner { agent: &mut agent, identity: &identity };
            (certificate_id(cert)?, session.authenticate_certificate_with(params.username.as_str(), cert.clone(), hash, &mut signer).await)
        } else {
            if !state.claim_key(&key) {
                continue;
            }
            (
                key.fingerprint(HashAlg::Sha256).to_string(),
                session.authenticate_publickey_with(params.username.as_str(), key.clone(), hash, &mut agent).await,
            )
        };
        match result {
            Ok(result) => {
                state.record_key(&result, &key);
                match record(result, state) {
                    KeyAttempt::Accepted => return Ok(true),
                    KeyAttempt::Rejected { keep_trying: false } => return Ok(false),
                    _ => {}
                }
            }
            Err(e) => {
                // A signer failure submitted no signed authentication: another source may
                // still hold the usable private key. The finite source lists bound retries.
                state.attempted_keys.remove(&fingerprint);
                log::warn!("agent key {comment:?} could not sign: {e}");
            }
        }
    }
    Ok(false)
}

/// An explicitly selected certificate may have only its bare private key in the agent.
/// Keep the certificate in the SSH auth payload, but address the identity the agent listed.
struct IdentitySigner<'a, S: AsyncRead + AsyncWrite> {
    agent: &'a mut AgentClient<S>,
    identity: &'a AgentIdentity,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> russh::Signer for IdentitySigner<'_, S> {
    type Error = russh::AgentAuthError;

    async fn auth_sign(&mut self, _key: &AgentIdentity, hash: Option<HashAlg>, data: Vec<u8>) -> Result<Vec<u8>, Self::Error> {
        Ok(self.agent.sign_request(self.identity, hash, data).await?)
    }
}

// -------------------------------------------------------------------------------------------------
// Passwords and keyboard-interactive

async fn passwords(session: &mut Session, params: &ConnectParams, sink: &EventSink, state: AuthState) -> anyhow::Result<()> {
    let AuthState { mut methods, mut second_factor, .. } = state;
    let user = params.username.as_str();
    // Once a method was accepted but the server wants more (`partial success`), what follows
    // is a second factor such as a one-time code: the stored password must not be spent on it,
    // and what the user types there must not replace the saved password.
    if let Some(password) = &params.password
        && !second_factor
        && allows(&methods, MethodKind::Password)
    {
        match session.authenticate_password(user, password.as_str()).await? {
            AuthResult::Success => return Ok(()),
            AuthResult::Failure { remaining_methods, partial_success } => {
                methods = remaining_methods;
                second_factor |= partial_success;
            }
        }
    }
    // A stored password may also answer a single keyboard-interactive password prompt
    // (servers with `PasswordAuthentication no` but PAM keyboard-interactive).
    let mut autofill = if second_factor { None } else { params.password.clone() };

    let target = format!("{user}@{}", params.host);
    // Some servers advertise keyboard-interactive but reject it without asking anything;
    // after that we fall back to plain password prompts.
    let mut keyboard_interactive_usable = true;
    let mut attempts = 0;
    while attempts < MAX_AUTH_ROUNDS {
        let outcome = if keyboard_interactive_usable && allows(&methods, MethodKind::KeyboardInteractive) {
            match keyboard_interactive(session, user, &target, &mut autofill, second_factor, sink).await? {
                Outcome::Unavailable(remaining, partial_success) => {
                    keyboard_interactive_usable = false;
                    second_factor |= partial_success;
                    if second_factor {
                        autofill = None;
                    }
                    methods = remaining;
                    continue;
                }
                outcome => outcome,
            }
        } else if allows(&methods, MethodKind::Password) {
            let Some(answers) = sink
                .prompt(
                    tr!("common.password_for", target = target),
                    String::new(),
                    vec![PromptField { label: t!("common.password").into(), echo: false }],
                    Some(user),
                    !second_factor,
                )
                .await
            else {
                bail!(t!("common.authentication_cancelled"));
            };
            let password = answers.into_iter().next().unwrap_or_default();
            match session.authenticate_password(user, password).await? {
                AuthResult::Success => Outcome::Success,
                AuthResult::Failure { remaining_methods, partial_success } => Outcome::Failure(remaining_methods, partial_success),
            }
        } else {
            let names: Vec<&str> = methods.iter().map(<&str>::from).collect();
            bail!(tr!("ssh_auth.authentication_failed_the_remaining_methods_are_not_available", arg0 = names.join(", ")));
        };

        attempts += 1;
        match outcome {
            Outcome::Success => return Ok(()),
            Outcome::Cancelled => bail!(t!("common.authentication_cancelled")),
            Outcome::Failure(remaining, true) => {
                sink.notice(t!("ssh_auth.further_authentication_is_required"));
                second_factor = true;
                autofill = None;
                methods = remaining;
            }
            Outcome::Unavailable(remaining, _) | Outcome::Failure(remaining, false) => {
                sink.notice(t!("common.authentication_failed_try_again"));
                methods = remaining;
            }
        }
    }
    bail!(t!("common.authentication_failed_too_many_attempts"))
}

enum Outcome {
    Success,
    /// Not (yet) logged in; `true` when the answer was accepted but more is required.
    Failure(MethodSet, bool),
    /// Rejected before any question was asked: the method is not really available.
    Unavailable(MethodSet, bool),
    Cancelled,
}

async fn keyboard_interactive(
    session: &mut Session,
    user: &str,
    target: &str,
    autofill: &mut Option<crate::Password>,
    second_factor: bool,
    sink: &EventSink,
) -> anyhow::Result<Outcome> {
    let mut response = session.authenticate_keyboard_interactive_start(user, None::<String>).await?;
    let mut asked = false;
    loop {
        response = match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(Outcome::Success),
            KeyboardInteractiveAuthResponse::Failure { remaining_methods, partial_success } => {
                return Ok(if asked {
                    Outcome::Failure(remaining_methods, partial_success)
                } else {
                    Outcome::Unavailable(remaining_methods, partial_success)
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                asked |= !prompts.is_empty();
                let answers = if prompts.is_empty() {
                    Vec::new()
                } else if let [only] = prompts.as_slice()
                    && !only.echo
                    && password_question(&only.prompt)
                    && let Some(password) = autofill.take()
                {
                    // russh owns String responses at this protocol boundary.
                    vec![password.as_str().to_owned()]
                } else {
                    let title = if name.is_empty() { tr!("ssh_auth.authentication_for", target = target) } else { name };
                    let fields: Vec<PromptField> =
                        prompts.into_iter().map(|p| PromptField { label: p.prompt.trim().to_owned(), echo: p.echo }).collect();
                    // Ambiguous hidden prompts (OTP, PIN, etc.) must not save or autofill a password.
                    let account_password =
                        !second_factor && matches!(fields.as_slice(), [only] if !only.echo && password_question(&only.label));
                    match sink.prompt(title, instructions, fields, Some(user), account_password).await {
                        Some(answers) => answers,
                        None => return Ok(Outcome::Cancelled),
                    }
                };
                session.authenticate_keyboard_interactive_respond(answers).await?
            }
        };
    }
}

fn password_question(label: &str) -> bool {
    matches!(label.trim().to_ascii_lowercase().trim_end_matches(':').trim(), "password" | "비밀번호")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_hidden_questions_are_not_saved_passwords() {
        assert!(password_question("Password: "));
        assert!(password_question("비밀번호:"));
        for label in ["Verification code:", "One-time password:", "OTP", "PIN:", "Challenge:"] {
            assert!(!password_question(label), "{label}");
        }
    }

    #[test]
    fn key_partial_success_survives_later_failures() {
        let mut state = AuthState {
            methods: MethodSet::from(&[MethodKind::PublicKey][..]),
            second_factor: false,
            attempted_keys: HashSet::new(),
            accepted_keys: HashSet::new(),
        };
        let result = record(
            AuthResult::Failure { remaining_methods: MethodSet::from(&[MethodKind::KeyboardInteractive][..]), partial_success: true },
            &mut state,
        );
        assert!(matches!(result, KeyAttempt::Rejected { keep_trying: false }));
        record(AuthResult::Failure { remaining_methods: MethodSet::from(&[MethodKind::Password][..]), partial_success: false }, &mut state);
        assert!(state.second_factor);
    }

    #[test]
    fn partial_success_allows_another_public_key_until_the_server_changes_methods() {
        let mut state = AuthState {
            methods: MethodSet::from(&[MethodKind::PublicKey][..]),
            second_factor: false,
            attempted_keys: HashSet::new(),
            accepted_keys: HashSet::new(),
        };
        for partial_success in [true, false] {
            let result = record(
                AuthResult::Failure { remaining_methods: MethodSet::from(&[MethodKind::PublicKey][..]), partial_success },
                &mut state,
            );
            assert!(matches!(result, KeyAttempt::Rejected { keep_trying: true }));
            assert!(state.second_factor);
        }
        let result = record(
            AuthResult::Failure { remaining_methods: MethodSet::from(&[MethodKind::KeyboardInteractive][..]), partial_success: true },
            &mut state,
        );
        assert!(matches!(result, KeyAttempt::Rejected { keep_trying: false }));
        assert!(state.second_factor);
    }

    #[test]
    fn detects_encrypted_key_formats() {
        let other = keys::Error::CouldNotReadKey;
        assert!(looks_encrypted("", &keys::Error::KeyIsEncrypted));
        assert!(looks_encrypted("-----BEGIN ENCRYPTED PRIVATE KEY-----", &other));
        assert!(looks_encrypted("Proc-Type: 4,ENCRYPTED", &other));
        assert!(looks_encrypted("PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: aes256-cbc\n", &other));
        assert!(!looks_encrypted("PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\n", &other));
        assert!(!looks_encrypted("garbage", &other));
    }

    #[test]
    fn unix_agent_sockets_fall_back_to_ssh_auth_sock() {
        let launchd = "/private/tmp/com.apple.launchd.x/Listeners";
        assert_eq!(unix_agent_sockets(None, Some(launchd)), [launchd]);
        assert_eq!(unix_agent_sockets(Some("/run/agent.sock"), Some(launchd)), ["/run/agent.sock", launchd]);
        // A pipe from a site made on Windows is no socket: only SSH_AUTH_SOCK is tried.
        assert_eq!(unix_agent_sockets(Some(r"\\.\pipe\openssh-ssh-agent"), Some(launchd)), [launchd]);
        assert_eq!(unix_agent_sockets(Some(launchd), Some(launchd)), [launchd], "tried once");
        assert!(unix_agent_sockets(Some(" "), Some("")).is_empty());
    }
}
