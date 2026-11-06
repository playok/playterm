//! Exercise the actual CLI with disposable keys and subprocess-local environment variables.
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    _dir: tempfile::TempDir,
    file: PathBuf,
    public: String,
}

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_playterm-sign"));
    command.env_remove("PLAYTERM_SIGNING_KEY").env_remove("PLAYTERM_SIGNING_KEY_FILE");
    command
}

fn keypair() -> (Vec<u8>, String) {
    let output = cli().arg("keygen").output().unwrap();
    assert!(output.status.success());
    let message = String::from_utf8(output.stderr).unwrap();
    let public = message.trim().strip_prefix("public key: ").unwrap().to_owned();
    (output.stdout, public)
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SHA256SUMS.txt");
        std::fs::write(&file, "test-checksum  fake-release.zip\n").unwrap();
        let (private, public) = keypair();
        let key_path = dir.path().join("disposable-test-key");
        std::fs::write(&key_path, private).unwrap();
        let signed = cli().arg("sign").arg(&file).env("PLAYTERM_SIGNING_KEY_FILE", &key_path).output().unwrap();
        assert!(signed.status.success(), "{}", String::from_utf8_lossy(&signed.stderr));
        Self { _dir: dir, file, public }
    }

    fn verify(&self, public: &str) -> Output {
        cli().arg("verify").arg(&self.file).arg(public).output().unwrap()
    }

    fn signature(&self) -> PathBuf {
        PathBuf::from(format!("{}.sig", self.file.display()))
    }
}

#[test]
fn accepts_the_generated_signature_and_its_public_key() {
    let fixture = Fixture::new();
    let result = fixture.verify(&fixture.public);
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("signature OK"));
}

#[test]
fn rejects_a_different_public_key() {
    let fixture = Fixture::new();
    let (_, wrong_public) = keypair();
    let result = fixture.verify(&wrong_public);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("BAD signature"));
}

#[test]
fn rejects_a_corrupted_but_hex_encoded_signature() {
    let fixture = Fixture::new();
    let mut signature = std::fs::read(fixture.signature()).unwrap();
    signature[0] = if signature[0] == b'0' { b'1' } else { b'0' };
    std::fs::write(fixture.signature(), signature).unwrap();
    let result = fixture.verify(&fixture.public);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("BAD signature"));
}

#[test]
fn rejects_modified_data_and_malformed_signature_text() {
    let fixture = Fixture::new();
    std::fs::write(&fixture.file, "changed data\n").unwrap();
    assert!(!fixture.verify(&fixture.public).status.success());
    std::fs::write(fixture.signature(), "not-hex\n").unwrap();
    let result = fixture.verify(&fixture.public);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("the signature is not hex"));
}
