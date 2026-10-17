#!/usr/bin/env bash
# Run on a Proxmox VE host: create an unprivileged Debian 13 LXC running TaiX
# headless, reachable at http://<ct-ip>:4040.
#
#   ./taix-lxc.sh BIN_DIR [CTID]
#
# BIN_DIR holds taix and taix-gui from `cargo build --release -p taix --features gui`.
# Re-run with the same CTID to update the binaries in place.
# Env: STORAGE (local-lvm), BRIDGE (vmbr0), MEMORY (2048), CORES (2), DISK (16).
set -euo pipefail

bin=${1:?usage: $0 BIN_DIR [CTID]}
here=$(cd "$(dirname "$0")" && pwd)
for f in taix taix-gui; do [[ -x $bin/$f ]] || { echo "missing $bin/$f" >&2; exit 1; }; done
ct=${2:-$(pvesh get /cluster/nextid)}

if ! pct status "$ct" &>/dev/null; then
  pveam update >/dev/null
  tpl=$(pveam available --section system | awk -v a="_$(dpkg --print-architecture)." '/debian-13-standard/ && index($2, a) {print $2}' | sort -V | tail -n1)
  pveam list local | grep -q "$tpl" || pveam download local "$tpl"
  pct create "$ct" "local:vztmpl/$tpl" \
    --hostname taix --unprivileged 1 --features nesting=1 --onboot 1 \
    --cores "${CORES:-2}" --memory "${MEMORY:-2048}" --swap 512 \
    --rootfs "${STORAGE:-local-lvm}:${DISK:-16}" \
    --net0 "name=eth0,bridge=${BRIDGE:-vmbr0},ip=dhcp" \
    --dev0 /dev/net/tun
  # A half-built container is worse than none: drop it if a later step fails.
  trap 'echo "setup failed; destroying CT $ct" >&2; pct stop "$ct" &>/dev/null; pct destroy "$ct" --purge' ERR
  pct start "$ct"
  for _ in $(seq 30); do pct exec "$ct" -- getent hosts deb.debian.org &>/dev/null && break; sleep 1; done
  pct exec "$ct" -- bash -c 'export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq && apt-get install -y -qq --no-install-recommends \
      tmux git ca-certificates curl unzip zsh sway libgtk-4-1 libadwaita-1-0 \
      libwebkitgtk-6.0-4 adwaita-icon-theme fonts-jetbrains-mono >/dev/null'
fi

# Shell and agent, each skipped when already there: zsh with oh-my-zsh as
# root's login shell (what every TaiX terminal starts), and Oh My Pi (omp).
pct exec "$ct" -- bash -c 'set -e
  command -v zsh >/dev/null || { apt-get install -y -qq zsh unzip >/dev/null; }
  [ -d /root/.oh-my-zsh ] || RUNZSH=no CHSH=no sh -c "$(curl -fsSL https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh)" >/dev/null
  chsh -s /usr/bin/zsh root
  command -v omp >/dev/null || [ -x /root/.local/bin/omp ] || curl -fsSL https://omp.sh/install | sh >/dev/null
  # omp lands in ~/.local/bin, which no shell here has on PATH.
  ln -sf /root/.local/bin/omp /usr/local/bin/omp'

pct exec "$ct" -- mkdir -p /etc/taix
pct exec "$ct" -- bash -c 'systemctl stop taix-host 2>/dev/null; pkill -x taix-gui; true'
pct push "$ct" "$bin/taix" /usr/local/bin/taix --perms 755
pct push "$ct" "$bin/taix-gui" /usr/local/bin/taix-gui --perms 755
pct push "$ct" "$here/taix-host.service" /etc/systemd/system/taix-host.service
pct push "$ct" "$here/taix-tmux.service" /etc/systemd/system/taix-tmux.service
# Nobody looks at this screen: a small one keeps GTK's shm buffers (one per
# frame in flight) small. The window exits, sway goes with it, systemd
# restarts both.
printf '%s\n' 'output * mode 800x600' 'exec "/usr/local/bin/taix-gui; swaymsg exit"' >/tmp/taix-sway.conf
pct push "$ct" /tmp/taix-sway.conf /etc/taix/sway.conf
pct exec "$ct" -- bash -c 'systemctl daemon-reload && systemctl enable --now taix-tmux taix-host'

for _ in $(seq 30); do pct exec "$ct" -- test -s /root/.local/share/taix/web.key && break; sleep 1; done
ip=$(pct exec "$ct" -- hostname -I | awk '{print $1}')
key=$(pct exec "$ct" -- cat /root/.local/share/taix/web.key)
echo "TaiX in CT $ct: http://$ip:4040/?k=$key"
echo "Add a project: pct exec $ct -- /usr/local/bin/taix add /root/<dir>"
