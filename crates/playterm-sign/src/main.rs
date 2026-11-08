//! Signs `SHA256SUMS.txt` of a release so playterm's updater can tell it comes from us.
//!
//!   playterm-sign keygen               new key pair: the private key (hex) on stdout, to keep
//!                                      secret; the public key on stderr, for
//!                                      `crates/playterm/src/update.rs`
//!   playterm-sign sign <file>          writes <file>.sig (hex Ed25519 signature of the file)
//!   playterm-sign verify <file> <hex>  checks <file>.sig against a public key
//!
//! The private key (hex PKCS#8) comes from `PLAYTERM_SIGNING_KEY`, or from the file named by
//! `PLAYTERM_SIGNING_KEY_FILE`.

use std::io::Read as _;
use std::process::ExitCode;

use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use zeroize::Zeroizing;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["keygen"] => keygen(),
        ["sign", file] => sign(file),
        ["verify", file, public] => verify(file, public),
        _ => Err("usage: playterm-sign keygen | sign <file> | verify <file> <public key hex>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("playterm-sign: {e}");
            ExitCode::FAILURE
        }
    }
}

fn keygen() -> Result<(), String> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| "key generation failed")?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "generated key is unusable")?;
    let secret_hex = Zeroizing::new(hex(pkcs8.as_ref()));
    println!("{}", secret_hex.as_str());
    eprintln!("public key: {}", hex(pair.public_key().as_ref()));
    Ok(())
}

fn sign(file: &str) -> Result<(), String> {
    let key = match (std::env::var("PLAYTERM_SIGNING_KEY").map(Zeroizing::new), std::env::var("PLAYTERM_SIGNING_KEY_FILE")) {
        (Ok(key), _) if !key.trim().is_empty() => key,
        (_, Ok(path)) if !path.is_empty() => {
            let mut key = Zeroizing::new(String::new());
            std::fs::File::open(&path).and_then(|mut file| file.read_to_string(&mut key)).map_err(|e| format!("{path}: {e}"))?;
            key
        }
        _ => return Err("set PLAYTERM_SIGNING_KEY or PLAYTERM_SIGNING_KEY_FILE".into()),
    };
    let pkcs8 = unhex(key.trim()).ok_or("the signing key is not hex")?;
    let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| "the signing key is not an Ed25519 PKCS#8 key")?;
    let data = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let signature = pair.sign(&data);
    let out = format!("{file}.sig");
    std::fs::write(&out, format!("{}\n", hex(signature.as_ref()))).map_err(|e| format!("{out}: {e}"))?;
    eprintln!("signed {file} -> {out} (public key {})", hex(pair.public_key().as_ref()));
    Ok(())
}

fn verify(file: &str, public: &str) -> Result<(), String> {
    let public = unhex(public).ok_or("the public key is not hex")?;
    let data = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
    let sig_path = format!("{file}.sig");
    let sig = std::fs::read_to_string(&sig_path).map_err(|e| format!("{sig_path}: {e}"))?;
    let sig = unhex(sig.trim()).ok_or("the signature is not hex")?;
    UnparsedPublicKey::new(&ED25519, public.as_slice()).verify(&data, &sig).map_err(|_| "BAD signature".to_owned())?;
    eprintln!("signature OK");
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

fn unhex(s: &str) -> Option<Zeroizing<Vec<u8>>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(s.len() / 2));
    for i in (0..s.len()).step_by(2) {
        bytes.push(u8::from_str_radix(s.get(i..i + 2)?, 16).ok()?);
    }
    Some(bytes)
}
