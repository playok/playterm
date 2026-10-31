//! SSH user authentication, tried in roughly the order OpenSSH uses:
//!
//! 1. `none` (learns which methods the server accepts)
//! 2. the explicitly configured private key file
//! 3. keys held by an SSH agent (Windows OpenSSH agent pipe, then Pageant)
//! 4. default key files (`~/.ssh/id_ed25519`, ...)
//! 5. the stored password
//! 6. keyboard-interactive / password prompts

use std::path::Path;
use std::sync::Arc;

use anyhow::bail;
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{self, Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg};
use russh::{MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncWrite};
use zeroize::Zeroizing;

use crate::session::{ConnectParams, EventSink, PromptField};
use crate::ssh::ClientHandler;

type Session = client::Handle<ClientHandler>;

const MAX_AUTH_ROUNDS: usize = 3;
const MAX_PASSPHRASE_TRIES: usize = 3;
/// Pipe served by the Windows OpenSSH `ssh-agent` service.
const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

fn allows(methods: &MethodSet, kind: MethodKind) -> bool {
    methods.contains(&kind)
}

pub(crate) async fn authenticate(session: &mut Session, params: &ConnectParams, sink: &EventSink) -> anyhow::Result<()> {
    let user = params.username.as_str();
    if user.is_empty() {
        bail!("SSH 사용자 이름이 필요합니다");
    }

    let mut methods = match session.authenticate_none(user).await? {
        AuthResult::Success => return Ok(()),
        AuthResult::Failure { remaining_methods, .. } => remaining_methods,
    };

    if allows(&methods, MethodKind::PublicKey) && public_keys(session, params, sink, &mut methods).await? {
        return Ok(());
    }
    passwords(session, params, sink, methods).await
}

// -------------------------------------------------------------------------------------------------
// Public keys

/// Outcome of one public-key attempt.
enum KeyAttempt {
    Accepted,
    /// Rejected; `false` once the server no longer offers public-key auth.
    Rejected { keep_trying: bool },
    /// The key could not be used (missing file, cancelled passphrase, agent refused).
    Unusable,
}

async fn public_keys(
    session: &mut Session,
    params: &ConnectParams,
    sink: &EventSink,
    methods: &mut MethodSet,
) -> anyhow::Result<bool> {
    if let Some(path) = &params.identity_file {
        match key_file(session, params, sink, path, true, methods).await? {
            KeyAttempt::Accepted => return Ok(true),
            KeyAttempt::Rejected { keep_trying: false } => return Ok(false),
            _ => {}
        }
    }

    if params.use_agent && agents(session, params, methods).await? {
        return Ok(true);
    }
    if !allows(methods, MethodKind::PublicKey) {
        return Ok(false);
    }

    for path in &params.default_identity_files {
        match key_file(session, params, sink, path, false, methods).await? {
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

fn record(result: AuthResult, methods: &mut MethodSet) -> KeyAttempt {
    match result {
        AuthResult::Success => KeyAttempt::Accepted,
        AuthResult::Failure { remaining_methods, .. } => {
            *methods = remaining_methods;
            KeyAttempt::Rejected { keep_trying: allows(methods, MethodKind::PublicKey) }
        }
    }
}

async fn key_file(
    session: &mut Session,
    params: &ConnectParams,
    sink: &EventSink,
    path: &Path,
    explicit: bool,
    methods: &mut MethodSet,
) -> anyhow::Result<KeyAttempt> {
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => Zeroizing::new(text),
        Err(e) => {
            if explicit {
                sink.notice(format!("키 파일을 읽을 수 없습니다 ({}): {e}", path.display()));
            }
            return Ok(KeyAttempt::Unusable);
        }
    };
    let Some(key) = decode_key(&text, path, sink).await else {
        return Ok(KeyAttempt::Unusable);
    };
    let hash = rsa_hash(session, key.algorithm()).await?;
    let result = session
        .authenticate_publickey(params.username.as_str(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await?;
    Ok(record(result, methods))
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

/// Decodes a private key, asking for its passphrase if needed. `None` = skip this key.
async fn decode_key(text: &str, path: &Path, sink: &EventSink) -> Option<PrivateKey> {
    match keys::decode_secret_key(text, None) {
        Ok(key) => return Some(key),
        Err(e) if !looks_encrypted(text, &e) => {
            sink.notice(format!("키 파일을 해석할 수 없습니다 ({}): {e}", path.display()));
            return None;
        }
        Err(_) => {}
    }

    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for _ in 0..MAX_PASSPHRASE_TRIES {
        // Cancelling skips this key; authentication continues with the next method.
        let answers = sink
            .prompt(
                format!("키 암호: {name}"),
                path.display().to_string(),
                vec![PromptField { label: "암호 (passphrase)".into(), echo: false }],
            )
            .await?;
        let passphrase = Zeroizing::new(answers.into_iter().next().unwrap_or_default());
        match keys::decode_secret_key(text, Some(&passphrase)) {
            Ok(key) => return Some(key),
            Err(_) => sink.notice(format!("{name}: 암호가 틀렸습니다")),
        }
    }
    None
}

/// Tries every key of the reachable agents. Returns `true` on success.
async fn agents(session: &mut Session, params: &ConnectParams, methods: &mut MethodSet) -> anyhow::Result<bool> {
    #[cfg(windows)]
    {
        // An explicitly configured pipe (or SSH_AUTH_SOCK naming a pipe, as set by e.g.
        // 1Password or KeePassXC) replaces the default OpenSSH service pipe.
        let configured = params.agent_pipe.clone().or_else(|| {
            std::env::var("SSH_AUTH_SOCK").ok().filter(|s| s.starts_with(r"\\.\pipe\"))
        });
        let pipe = configured.clone().unwrap_or_else(|| OPENSSH_AGENT_PIPE.to_owned());
        match AgentClient::connect_named_pipe(&pipe).await {
            Ok(mut agent) => {
                if let Some(binding) = session.agent_session_binding() {
                    // Older agents do not know session-bind; that only disables
                    // destination-constrained keys, so the error is ignored.
                    let _ = agent.bind_session(binding, false).await;
                }
                if agent_keys(session, params, agent, methods).await? {
                    return Ok(true);
                }
            }
            Err(e) => log::debug!("no SSH agent at {pipe}: {e}"),
        }

        if params.agent_pipe.is_none() && allows(methods, MethodKind::PublicKey) {
            match AgentClient::connect_pageant().await {
                Ok(agent) => {
                    if agent_keys(session, params, agent, methods).await? {
                        return Ok(true);
                    }
                }
                Err(e) => log::debug!("Pageant not available: {e}"),
            }
        }
    }
    #[cfg(not(windows))]
    let _ = (session, params, methods);
    Ok(false)
}

async fn agent_keys<S>(
    session: &mut Session,
    params: &ConnectParams,
    mut agent: AgentClient<S>,
    methods: &mut MethodSet,
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
    for identity in identities {
        // Certificates need a different auth request; plain keys cover the common case.
        let AgentIdentity::PublicKey { key, comment } = identity else { continue };
        let hash = rsa_hash(session, key.algorithm()).await?;
        match session.authenticate_publickey_with(params.username.as_str(), key, hash, &mut agent).await {
            Ok(result) => match record(result, methods) {
                KeyAttempt::Accepted => return Ok(true),
                KeyAttempt::Rejected { keep_trying: false } => return Ok(false),
                _ => {}
            },
            Err(e) => log::warn!("agent key {comment:?} could not sign: {e}"),
        }
    }
    Ok(false)
}

// -------------------------------------------------------------------------------------------------
// Passwords and keyboard-interactive

async fn passwords(
    session: &mut Session,
    params: &ConnectParams,
    sink: &EventSink,
    mut methods: MethodSet,
) -> anyhow::Result<()> {
    let user = params.username.as_str();
    if let Some(password) = &params.password
        && allows(&methods, MethodKind::Password)
    {
        match session.authenticate_password(user, password).await? {
            AuthResult::Success => return Ok(()),
            AuthResult::Failure { remaining_methods, .. } => methods = remaining_methods,
        }
    }
    // A stored password may also answer a single keyboard-interactive password prompt
    // (servers with `PasswordAuthentication no` but PAM keyboard-interactive).
    let mut autofill = params.password.clone();

    let target = format!("{user}@{}", params.host);
    // Some servers advertise keyboard-interactive but reject it without asking anything;
    // after that we fall back to plain password prompts.
    let mut keyboard_interactive_usable = true;
    let mut attempts = 0;
    while attempts < MAX_AUTH_ROUNDS {
        let outcome = if keyboard_interactive_usable && allows(&methods, MethodKind::KeyboardInteractive) {
            match keyboard_interactive(session, user, &target, &mut autofill, sink).await? {
                Outcome::Unavailable(remaining) => {
                    keyboard_interactive_usable = false;
                    methods = remaining;
                    continue;
                }
                outcome => outcome,
            }
        } else if allows(&methods, MethodKind::Password) {
            let Some(answers) = sink
                .prompt(
                    format!("{target} 비밀번호"),
                    String::new(),
                    vec![PromptField { label: "비밀번호".into(), echo: false }],
                )
                .await
            else {
                bail!("인증이 취소되었습니다");
            };
            let password = answers.into_iter().next().unwrap_or_default();
            match session.authenticate_password(user, password).await? {
                AuthResult::Success => Outcome::Success,
                AuthResult::Failure { remaining_methods, .. } => Outcome::Failure(remaining_methods),
            }
        } else {
            let names: Vec<&str> = methods.iter().map(<&str>::from).collect();
            bail!("인증 실패: 남은 인증 방식({})을 사용할 수 없습니다", names.join(", "));
        };

        attempts += 1;
        match outcome {
            Outcome::Success => return Ok(()),
            Outcome::Cancelled => bail!("인증이 취소되었습니다"),
            Outcome::Unavailable(remaining) | Outcome::Failure(remaining) => {
                sink.notice("인증 실패, 다시 시도하세요.");
                methods = remaining;
            }
        }
    }
    bail!("인증 실패: 시도 횟수를 초과했습니다")
}

enum Outcome {
    Success,
    Failure(MethodSet),
    /// Rejected before any question was asked: the method is not really available.
    Unavailable(MethodSet),
    Cancelled,
}

async fn keyboard_interactive(
    session: &mut Session,
    user: &str,
    target: &str,
    autofill: &mut Option<String>,
    sink: &EventSink,
) -> anyhow::Result<Outcome> {
    let mut response = session.authenticate_keyboard_interactive_start(user, None::<String>).await?;
    let mut asked = false;
    loop {
        response = match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(Outcome::Success),
            KeyboardInteractiveAuthResponse::Failure { remaining_methods, .. } => {
                return Ok(if asked {
                    Outcome::Failure(remaining_methods)
                } else {
                    Outcome::Unavailable(remaining_methods)
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                asked |= !prompts.is_empty();
                let answers = if prompts.is_empty() {
                    Vec::new()
                } else if let [only] = prompts.as_slice()
                    && !only.echo
                    && let Some(password) = autofill.take()
                {
                    vec![password]
                } else {
                    let title = if name.is_empty() { format!("{target} 인증") } else { name };
                    let fields = prompts
                        .into_iter()
                        .map(|p| PromptField { label: p.prompt.trim().to_owned(), echo: p.echo })
                        .collect();
                    match sink.prompt(title, instructions, fields).await {
                        Some(answers) => answers,
                        None => return Ok(Outcome::Cancelled),
                    }
                };
                session.authenticate_keyboard_interactive_respond(answers).await?
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
