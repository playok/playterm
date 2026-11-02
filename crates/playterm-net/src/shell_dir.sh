# Prints the current directory of the interactive shell of this SSH connection.
#
# Run by playterm in a second channel of the same connection (`sh -s`, script on stdin): every
# channel of a connection is a child of the same sshd process, so the shell with a terminal
# among its children is the one the user is typing into. Its directory is read from /proc,
# following nested shells (bash -> bash) as far as they can be read. Linux only; elsewhere it
# prints nothing and fails.

# Up from this script to the sshd process; `q` is its child on our side (this channel).
p=$$
q=
while :; do
    c=$(ps -o comm= -p "$p" 2>/dev/null)
    case "$c" in
        sshd* | dropbear*) break ;;
    esac
    q=$p
    p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ')
    [ -n "$p" ] && [ "$p" -gt 1 ] || exit 1
done

s=$(ps -o pid=,tty= --ppid "$p" 2>/dev/null | awk -v q="$q" '$2 != "?" && $1 != q { print $1; exit }')
[ -n "$s" ] || exit 1

while :; do
    c=$(ps -o pid=,comm= --ppid "$s" 2>/dev/null | awk '$2 ~ /^-?(ba|z|k|mk|da|fi|tc|c|a)?sh$/ { print $1; exit }')
    [ -n "$c" ] && [ -d "/proc/$c/cwd" ] || break
    s=$c
done

readlink "/proc/$s/cwd"
