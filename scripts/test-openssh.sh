#!/bin/sh
# Linux CI only: a disposable test account and daemon, never a production server.
set -eu
[ "${CI:-}" = true ] || { echo 'Run only on a disposable CI runner' >&2; exit 1; }
fixture=$(mktemp -d)
cleanup() {
    for pid_file in sshd.pid sshd-mfa.pid; do
        if [ -f "$fixture/$pid_file" ]; then sudo kill "$(cat "$fixture/$pid_file")" 2>/dev/null || true; fi
    done
    sudo userdel -r playterm-ci 2>/dev/null || true
    rm -rf "$fixture"
}
trap cleanup EXIT INT TERM
sudo useradd -m -s /bin/sh playterm-ci
# Fake local credentials unlock the disposable account. Tests authenticate using a generated key.
printf 'playterm-ci:local-test-only\n' | sudo chpasswd
chmod 755 "$fixture"
ssh-keygen -q -t ed25519 -N '' -f "$fixture/client"
ssh-keygen -q -t ed25519 -N '' -f "$fixture/host"
cp "$fixture/client.pub" "$fixture/authorized_keys"
chmod 644 "$fixture/authorized_keys"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
cat > "$fixture/sshd_config" <<EOF
ListenAddress 127.0.0.1
Port $port
HostKey $fixture/host
PidFile $fixture/sshd.pid
AuthorizedKeysFile $fixture/authorized_keys
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
AllowUsers playterm-ci
Subsystem sftp internal-sftp
LogLevel VERBOSE
EOF
sudo mkdir -p /run/sshd
sudo /usr/sbin/sshd -t -f "$fixture/sshd_config"
sudo /usr/sbin/sshd -f "$fixture/sshd_config" -E "$fixture/sshd.log"
export PLAYTERM_OPENSSH_PORT="$port" PLAYTERM_OPENSSH_USER=playterm-ci PLAYTERM_OPENSSH_KEY="$fixture/client"
/usr/sbin/sshd -V 2>&1 || true
cargo test --locked -p playterm-net --test openssh_interop openssh_key_shell_and_unicode_sftp -- --ignored --nocapture || { cat "$fixture/sshd.log"; exit 1; }

# A second real daemon requires key + PAM keyboard-interactive. Its PAM answer is deliberately
# the stored password, so accidental autofill succeeds silently and is caught by the test.
mfa_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
sed -e "s/^Port .*/Port $mfa_port/" \
    -e "s|^PidFile .*|PidFile $fixture/sshd-mfa.pid|" \
    -e 's/^KbdInteractiveAuthentication no/KbdInteractiveAuthentication yes/' \
    -e 's/^UsePAM no/UsePAM yes/' "$fixture/sshd_config" > "$fixture/sshd_mfa_config"
printf '\nAuthenticationMethods publickey,keyboard-interactive\n' >> "$fixture/sshd_mfa_config"
sudo /usr/sbin/sshd -t -f "$fixture/sshd_mfa_config"
sudo /usr/sbin/sshd -f "$fixture/sshd_mfa_config" -E "$fixture/sshd-mfa.log"
export PLAYTERM_OPENSSH_MFA_PORT="$mfa_port"
cargo test --locked -p playterm-net --test openssh_interop openssh_public_key_mfa_does_not_autofill_or_save_second_factor -- --ignored --nocapture || { cat "$fixture/sshd-mfa.log"; exit 1; }
