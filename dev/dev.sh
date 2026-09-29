#!/bin/bash
# Development helper: run Kiore on this Mac and on the iMac (ssh "$REMOTE") together.
#   dev/dev.sh start [log-filter]   rebuild both, restart both daemons
#   dev/dev.sh stop                 stop both
#   dev/dev.sh logs [n]             show the last n log lines from both
# The Linux machine to test against (an SSH host name or alias).
REMOTE=${KIORE_REMOTE:-omarchy}
set -u
cd "$(dirname "$0")/.."
filter=${2:-"debug,quinn=info,quinn_proto=info,rustls=info,mdns_sd=warn"}
strip() { sed 's/\x1b\[[0-9;]*m//g'; }

stop_mac() { pkill -x kiore; while pgrep -x kiore >/dev/null; do sleep 0.1; done; }
stop_imac() { ssh "$REMOTE" 'pkill -x kiore; while pgrep -x kiore >/dev/null; do sleep 0.1; done'; }

case ${1:-start} in
  start)
    cargo build -q -p kiore || exit 1
    # A stable signing identity keeps macOS permission and firewall decisions across rebuilds
    # (ad-hoc signatures change every build). Uses the first Apple Development identity.
    ident=$(security find-identity -v -p codesigning | awk '/Apple Development/ {print $2; exit}')
    [[ -n $ident ]] && codesign --force --sign "$ident" --identifier nz.galengreen.kiore \
      target/debug/kiore 2>/dev/null
    dev/sync.sh
    ssh "$REMOTE" 'cd ~/Workspace/Kiore && ~/.cargo/bin/cargo build -q -p kiore' || exit 1
    stop_mac
    if ssh "$REMOTE" 'systemctl --user cat kiore.service >/dev/null 2>&1'; then
      # Installed as a service (scripts/install-linux.sh): swap in the dev build and restart it.
      ssh "$REMOTE" "install -m755 ~/Workspace/Kiore/target/debug/kiore ~/.local/bin/kiore && systemctl --user set-environment RUST_LOG='$filter' && systemctl --user restart kiore"
    else
      stop_imac
      ssh "$REMOTE" "cd ~/Workspace/Kiore && . dev/remote-env.sh && (RUST_LOG='$filter' setsid nohup ./target/debug/kiore run > /tmp/kiore.log 2>&1 < /dev/null &)"
    fi
    (RUST_LOG="$filter" nohup ./target/debug/kiore run > /tmp/kiore-mac.log 2>&1 < /dev/null &)
    echo started
    ;;
  stop) stop_mac; stop_imac; echo stopped ;;
  logs)
    n=${2:-20}
    echo "== mac"; strip < /tmp/kiore-mac.log | tail -n "$n"
    echo "== imac"
    ssh "$REMOTE" "if systemctl --user cat kiore.service >/dev/null 2>&1; then journalctl --user -u kiore -n $n --no-pager -o cat; else tail -n $n /tmp/kiore.log; fi" | strip
    ;;
esac
