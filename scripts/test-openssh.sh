#!/bin/sh
# Linux CI only: a disposable test account and daemon, never a production server.
set -eu
[ "${CI:-}" = true ] || { echo 'Run only on a disposable CI runner' >&2; exit 1; }
fixture=$(mktemp -d)
test_agent_pid=
cleanup() {
    for pid_file in sshd.pid sshd-mfa.pid sshd-two.pid sshd-two-mfa.pid; do
        if [ -f "$fixture/$pid_file" ]; then sudo kill "$(cat "$fixture/$pid_file")" 2>/dev/null || true; fi
    done
    if [ -n "$test_agent_pid" ]; then kill "$test_agent_pid" 2>/dev/null || true; fi
    sudo userdel -r playterm-ci 2>/dev/null || true
    rm -rf "$fixture"
}
trap cleanup EXIT INT TERM
sudo useradd -m -s /bin/sh playterm-ci
# Fake local credentials unlock the disposable account. Tests authenticate using a generated key.
printf 'playterm-ci:local-test-only\n' | sudo chpasswd
chmod 755 "$fixture"
ssh-keygen -q -t ed25519 -N '' -f "$fixture/client"
ssh-keygen -q -t ed25519 -N '' -f "$fixture/client-b"
ssh-keygen -q -t ed25519 -N '' -f "$fixture/wrong"
ssh-keygen -q -t ed25519 -N '' -f "$fixture/host"
cat "$fixture/client.pub" "$fixture/client-b.pub" > "$fixture/authorized_keys"
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

# Two distinct keys, followed by an optional PAM factor. Keep all agents and credentials
# confined to the disposable fixture; do not reuse the runner's agent.
two_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
sed -e "s/^Port .*/Port $two_port/" \
    -e "s|^PidFile .*|PidFile $fixture/sshd-two.pid|" "$fixture/sshd_config" > "$fixture/sshd_two_config"
printf '\nAuthenticationMethods publickey,publickey\n' >> "$fixture/sshd_two_config"
sudo /usr/sbin/sshd -t -f "$fixture/sshd_two_config"
sudo /usr/sbin/sshd -f "$fixture/sshd_two_config" -E "$fixture/sshd-two.log"
two_mfa_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
sed -e "s/^Port .*/Port $two_mfa_port/" \
    -e "s|^PidFile .*|PidFile $fixture/sshd-two-mfa.pid|" \
    -e 's/AuthenticationMethods publickey,keyboard-interactive/AuthenticationMethods publickey,publickey,keyboard-interactive/' \
    "$fixture/sshd_mfa_config" > "$fixture/sshd_two_mfa_config"
sudo /usr/sbin/sshd -t -f "$fixture/sshd_two_mfa_config"
sudo /usr/sbin/sshd -f "$fixture/sshd_two_mfa_config" -E "$fixture/sshd-two-mfa.log"
eval "$(ssh-agent -s)" >/dev/null
test_agent_pid=$SSH_AGENT_PID
ssh-add "$fixture/client" "$fixture/client-b"
# Independent OpenSSH controls: A+B succeeds, A alone fails with agent/default keys excluded.
ssh -F /dev/null -p "$two_port" -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none \
    -o PreferredAuthentications=publickey -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$fixture/known_hosts" \
    -i "$fixture/client" -i "$fixture/client-b" playterm-ci@127.0.0.1 true
if ssh -F /dev/null -p "$two_port" -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none \
    -o PreferredAuthentications=publickey -o UserKnownHostsFile="$fixture/known_hosts" \
    -i "$fixture/client" playterm-ci@127.0.0.1 true; then
    echo 'A single key unexpectedly completed two-key authentication' >&2
    exit 1
fi
export PLAYTERM_OPENSSH_TWO_KEY_PORT="$two_port" PLAYTERM_OPENSSH_TWO_KEY_MFA_PORT="$two_mfa_port"
export PLAYTERM_OPENSSH_KEY_B="$fixture/client-b" PLAYTERM_OPENSSH_WRONG_KEY="$fixture/wrong"
cargo test --locked -p playterm-net --test openssh_interop openssh_two_public_keys_and_two_keys_then_mfa -- --ignored --nocapture || {
    cat "$fixture/sshd-two.log" "$fixture/sshd-two-mfa.log"; exit 1;
}
cat "$fixture/sshd-two.log" "$fixture/sshd-two-mfa.log"
