# Live session only. Over SSH, what runs from this login ignores hangups: a
# lost connection never interrupts an installation, which runs to its end
# without a terminal. When the session runs the interactive installer (it
# made a password for SSH), the installer starts at once; with an
# autoinstall file, SSH is for watching it.
if [ -n "$SSH_CONNECTION" ]; then
    trap '' HUP
    if [ -t 0 ] && [ -z "$DELI_SHELL" ] && [ -e /run/deli/ssh-password ]; then
        export DELI_SHELL=1
        deli install
    fi
fi
