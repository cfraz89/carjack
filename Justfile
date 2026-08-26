set dotenv-load

# Is the targetedd unit reachable? 
[script('bash')]
reachable device=env('CARJACK.BLUETOOTH.DEVICE', ''):
    set -uo pipefail
    if [ -z "{{device}}" ]; then echo "no head unit configured" >&2; exit 2; fi
    echo "Scanning for {{device}} ..." >&2
    if timeout 25s bluetoothctl --timeout 18 scan on 2>/dev/null | grep -qiF "{{device}}"; then
        echo "present" >&2
    else
        echo "NOT present - off, asleep, or out of range" >&2
        exit 1
    fi

# Hand the hands-free gateway UUID over to carjack, and keep the bluez monitor running on a
# headless seat - see wireplumber/51-carjack-hfp-ag.conf and wireplumber/52-carjack-bluez-seat.conf.
[script]
install-hfp-config:
    dir="${XDG_CONFIG_HOME:-$HOME/.config}/wireplumber/wireplumber.conf.d"
    mkdir -p "$dir"
    for conf in wireplumber/*.conf; do
        install -m644 "$conf" "$dir/$(basename "$conf")"
        echo "Installed $dir/$(basename "$conf")"
    done
    systemctl --user restart wireplumber
    echo "Remove them and restart wireplumber to undo"

toolbox_build := if env('TOOLBOX_PATH', '') != '' { '' } else if env('TOOLBOX_BUILD', '') == 'true' { 'toolbox run ' } else { '' }

#Needed for nvidia zero-copy support, but also needs some extra deps
cuda := "false"
cuda_features := if cuda == 'true' { '--features cuda' } else { '' }

#run carjack
run *flags: (build flags)
    waydroid session stop
    cargo run {{cuda_features}} {{flags}}

#Run the app until timeout, then kill it and restore the original wifi connection - networkmanager only
[script('bash')]
[arg("timeout", short="t", long)]
test-run timeout="60" *flags: (build flags)
    set -uo pipefail
    original=""
    cleanup() {
        # Anchored: unanchored, this matches *any* process whose command line merely contains the
        # path - a tail on the log, an editor, a wrapper script driving this recipe - and kills it.
        if [ -n "$original" ]; then
            current=$(nmcli -t -f NAME,ACTIVE connection show --active 2>/dev/null | awk -F: '$2=="yes"{print $1; exit}')
            if [ "$current" != "$original" ]; then
                echo "Restoring original connection: $original" >&2
                timeout --foreground --kill-after=5s 10s nmcli connection up id "$original" >/dev/null 2>&1 || true
            fi
        fi
    }
    trap cleanup EXIT
    trap cleanup ERR

    original=$(nmcli -t -f NAME,TYPE,ACTIVE connection show --active 2>/dev/null | awk -F: '$2!="loopback" && $3=="yes" {print $1; exit}')
    echo "Active connection before test: ${original:-none}" >&2

    waydroid session stop || true
    timeout --foreground --kill-after=5s "{{timeout}}s" cargo run {{cuda_features}} {{flags}} < /dev/null || true

#build carjack
[default]
build +flags="":
    {{toolbox_build}}cargo build {{cuda_features}} {{flags}}

#run cargo install
install +flags="": (build flags)
    {{toolbox_build}}cargo install --path . {{cuda_features}} {{flags}}

# Install locally as a systemd *user* service, running as whoever invokes this.
[script]
install-carjack-service: build
    sudo install -Dm755 target/debug/carjack /usr/local/bin/carjack
    sudo install -Dm644 carjack.service /usr/lib/systemd/user/carjack.service
    # The shipped unit names the deb's /usr/bin path; repoint it at the one we just wrote.
    printf '[Service]\nExecStart=\nExecStart=/usr/local/bin/carjack\n' \
        | install -Dm644 /dev/stdin "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/carjack.service.d/local-path.conf"
    # The polkit rule names a user; on a dev box that is you, not "carjack".
    sed "s/\"carjack\"/\"$USER\"/" carjack-polkit.rules \
        | sudo install -Dm644 /dev/stdin /etc/polkit-1/rules.d/49-carjack.rules
    systemctl --user daemon-reload
    systemctl --user enable --now carjack.service
    systemctl --user status carjack.service --no-pager

#Build an arm64 deb of carjack
deb:
    podman build -t carjack-deb-builder -f Containerfile.deb .
    podman run --rm --security-opt label=disable \
        -v {{justfile_directory()}}:/src \
        -v {{home_directory()}}/.cargo/registry:/usr/local/cargo/registry \
        -v {{home_directory()}}/.cargo/git:/usr/local/cargo/git \
        carjack-deb-builder cargo deb --offline -- {{cuda_features}}

# Run cargo tests
test +flags="":
    {{toolbox_build}}cargo test {{cuda_features}} {{flags}}

# The carjack binary's own unit tests. 
test-bin +flags="":
    {{toolbox_build}}cargo test --bin carjack {{cuda_features}} {{flags}} < /dev/null

# The iAP2 protocol unit tests (message encode/decode against bytes captured from a working
# iPhone session).
test-proto +flags="":
    cargo test -p iap2-proto {{flags}} < /dev/null

# The libcarjack unit tests (iAP2 session logic, against bytes captured from real head units).
test-lib +flags="":
    {{toolbox_build}}cargo test -p libcarjack --lib {{flags}} < /dev/null

install-cuda-dmabuf-deps:
    {{toolbox_build}}sudo dnf install -y libEGL-devel libgbm-devel gstreamer1-plugins-bad-free-devel

# Install dependencies on a fedora system - possibly in toolbox
install-deps:
    {{toolbox_build}}sudo dnf install gcc g++ gstreamer1-devel gstreamer1-plugins-base-devel pipewire-gstreamer dbus-devel \
    openssl-devel mesa-libgbm-devel libdrm-devel libxkbcommon-devel mesa-libEGL-devel mesa-libGLES-devel \
    libacl-devel
