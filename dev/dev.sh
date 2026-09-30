#!/bin/bash
# Development helper: run MouseTail on this Mac and on the iMac (ssh "$REMOTE") together.
#   dev/dev.sh start [log-filter]   rebuild both, restart both daemons
#   dev/dev.sh stop                 stop both
#   dev/dev.sh logs [n]             show the last n log lines from both
# The Linux machine to test against (an SSH host name or alias).
REMOTE=${MOUSETAIL_REMOTE:-omarchy}
set -u
cd "$(dirname "$0")/.."
filter=${2:-"debug,quinn=info,quinn_proto=info,rustls=info,mdns_sd=warn"}
strip() { sed 's/\x1b\[[0-9;]*m//g'; }

stop_mac() { pkill -x mousetail; while pgrep -x mousetail >/dev/null; do sleep 0.1; done; }
stop_imac() { ssh "$REMOTE" 'pkill -x mousetail; while pgrep -x mousetail >/dev/null; do sleep 0.1; done'; }

case ${1:-start} in
  start)
    if pgrep -x mousetaild >/dev/null; then
      echo "Quit the MouseTail app first: only one MouseTail can run at a time." >&2
      exit 1
    fi
    cargo build -q -p mousetail || exit 1
    # A stable signing identity keeps macOS permission and firewall decisions across rebuilds
    # (ad-hoc signatures change every build). Uses the first Apple Development identity.
    ident=$(security find-identity -v -p codesigning | awk '/Apple Development/ {print $2; exit}')
    [[ -n $ident ]] && codesign --force --sign "$ident" --identifier nz.galengreen.mousetail \
      target/debug/mousetail 2>/dev/null
    dev/sync.sh
    ssh "$REMOTE" 'cd ~/Workspace/MouseTail && ~/.cargo/bin/cargo build -q -p mousetail' || exit 1
    stop_mac
    if ssh "$REMOTE" 'systemctl --user cat mousetail.service >/dev/null 2>&1'; then
      # Installed as a service (scripts/install-linux.sh): swap in the dev build and restart it.
      ssh "$REMOTE" "install -m755 ~/Workspace/MouseTail/target/debug/mousetail ~/.local/bin/mousetail && systemctl --user set-environment RUST_LOG='$filter' && systemctl --user restart mousetail"
    else
      stop_imac
      ssh "$REMOTE" "cd ~/Workspace/MouseTail && . dev/remote-env.sh && (RUST_LOG='$filter' setsid nohup ./target/debug/mousetail run > /tmp/mousetail.log 2>&1 < /dev/null &)"
    fi
    (RUST_LOG="$filter" nohup ./target/debug/mousetail run > /tmp/mousetail-mac.log 2>&1 < /dev/null &)
    echo started
    ;;
  stop) stop_mac; stop_imac; echo stopped ;;
  logs)
    n=${2:-20}
    echo "== mac"; strip < /tmp/mousetail-mac.log | tail -n "$n"
    echo "== imac"
    ssh "$REMOTE" "if systemctl --user cat mousetail.service >/dev/null 2>&1; then journalctl --user -u mousetail -n $n --no-pager -o cat; else tail -n $n /tmp/mousetail.log; fi" | strip
    ;;
esac
