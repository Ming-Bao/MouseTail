#!/bin/bash
# Run the Linux-side spikes inside the live Hyprland session (invoked over SSH).
set -u
cd "$(dirname "$0")/.."
. dev/remote-env.sh
export OMARCHY_PATH=/usr/share/omarchy
bin=./target/debug

idle() { omarchy-shell idle status | jq -c '{idle, locked: .locked}' 2>/dev/null || omarchy-shell idle status; }

# Hyprland 0.56+ dispatch takes Lua; close by pid so nothing else can match.
close_test_window() { hyprctl clients -j | jq -r '.[] | select(.class == "bm-test") | .pid' | xargs -r kill; }

focus_test_window() {
  rm -f /tmp/bm-kbd
  hyprctl dispatch "hl.dsp.exec_cmd([[foot --app-id=bm-test $PWD/dev/kbd-sink.sh]])" >/dev/null
  for _ in $(seq 1 20); do
    sleep 0.25
    hyprctl activewindow -j | jq -e '.class == "bm-test"' >/dev/null && return 0
  done
  echo "active window: $(hyprctl activewindow -j | jq -r .class)"
  return 1
}

case ${1:-all} in
  pointer) $bin/spike-linux-inject pointer ;;
  latency) $bin/spike-linux-inject latency ;;
  clipboard) $bin/spike-linux-clipboard ;;
  keyboard)
    if focus_test_window; then
      $bin/spike-linux-inject type "MouseTail ok 123"
      sleep 0.5
      got=$(cat /tmp/bm-kbd 2>/dev/null)
      echo "received: '$got'"
      [[ $got == "MouseTail ok 123" ]] && echo "keyboard: PASS" || echo "keyboard: FAIL"
    else
      echo "keyboard: SKIPPED (test window never focused)"
    fi
    close_test_window
    ;;
  idle)
    # Phase 1: nudge every 30s for 200s; idle must stay false past the 150s screensaver.
    # Phase 2: stop; idle should flip true (control). Phase 3: one nudge must wake it.
    status() { omarchy-shell idle status | jq -c '{idle, screensaverStarted}'; }
    echo "$(date +%T) start $(status)"
    for i in $(seq 1 7); do
      $bin/spike-linux-inject nudge >/dev/null
      sleep 30
      echo "$(date +%T) nudged $i $(status)"
    done
    for i in $(seq 1 18); do
      sleep 10
      s=$(status); echo "$(date +%T) no input $s"
      [[ $s == *'"idle":true'* ]] && break
    done
    $bin/spike-linux-inject nudge >/dev/null
    sleep 2
    echo "$(date +%T) after wake nudge $(status)"
    ;;
  lock)
    # Needs you to unlock by hand afterwards. Never submits a password.
    lockstatus() { omarchy-shell lock status | jq -c '{locked, sessionLocked, secure}'; }
    omarchy-shell lock lock
    for _ in $(seq 1 40); do sleep 0.25; omarchy-shell lock status | jq -e '.secure' >/dev/null && break; done
    echo "lock: $(lockstatus)"
    sleep 1
    $bin/spike-linux-inject pointer
    $bin/spike-linux-inject type "abc" --no-enter
    sleep 0.5; grim /tmp/bm-lock-typed.png
    $bin/spike-linux-inject type "abc" --erase
    sleep 0.5; grim /tmp/bm-lock-erased.png
    echo "lock after: $(lockstatus)"
    ;;
  all)
    for t in pointer latency clipboard keyboard; do echo "== $t"; "$0" "$t"; done
    ;;
esac
