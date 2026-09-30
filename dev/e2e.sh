#!/bin/bash
# End-to-end checks with both daemons running (dev/dev.sh start) and the machines paired.
# Drives the Mac with synthetic HID events and checks the iMac over SSH.
# The Linux machine to test against (an SSH host name or alias).
REMOTE=${MOUSETAIL_REMOTE:-omarchy}
set -u
cd "$(dirname "$0")/.."
drive=./target/debug/spike-mac-drive
imac() { ssh "$REMOTE" ". ~/Workspace/MouseTail/dev/remote-env.sh; $*"; }
pos() { imac hyprctl cursorpos; }
state() { ./target/debug/mousetail status | grep -E "cursor is on" || echo "  cursor is home"; }
pass=0; fail=0
check() { if [[ $2 == "$3" ]]; then echo "  ok   $1 ($2)"; pass=$((pass+1)); else echo "  FAIL $1: got '$2', want '$3'"; fail=$((fail+1)); fi; }

case ${1:-motion} in
  motion)
    echo "== cross into the iMac at the MacBook's left edge"
    $drive warp 5 500; sleep 0.2
    $drive push -10 0 3; sleep 0.3
    check "controller says cursor is on the iMac" "$(state | xargs)" "cursor is on 5b9095fb21130aba"
    check "iMac cursor near right edge, height 500" "$(pos)" "1899, 500"
    check "Mac cursor frozen at the edge" "$($drive where)" "0, 500"
    echo "== move around on the iMac"
    $drive push -100 20 5; sleep 0.3
    check "iMac cursor moved" "$(pos)" "1399, 600"
    $drive push -1000 0 3; sleep 0.3
    check "clamped at iMac's far left edge" "$(pos)" "0, 600"
    echo "== come back"
    $drive push 200 0 11; sleep 0.3
    check "controller says cursor is home" "$(state | xargs)" "cursor is home"
    mac=$($drive where); mx=${mac%%,*}
    check "Mac cursor back on the MacBook (x > 0)" "$(( ${mx%.*} > 0 ))" "1"
    ;;
  keyboard)
    echo "== typing from the Mac lands in an iMac window"
    open_sink() {
      imac "rm -f $2; hyprctl dispatch 'hl.dsp.exec_cmd([[foot --app-id=mousetail-test \$HOME/Workspace/MouseTail/dev/$1]])' >/dev/null"
      for _ in $(seq 1 20); do sleep 0.25; imac "hyprctl activewindow -j | jq -e '.class == \"mousetail-test\"' >/dev/null" && return 0; done
      return 1
    }
    close_sink() { imac "hyprctl clients -j | jq -r '.[] | select(.class == \"mousetail-test\") | .pid' | xargs -r kill"; }
    # Approach the edge the way a mouse does, at a height where the iMac sits alongside.
    $drive warp 60 200; sleep 0.2; $drive push -10 0 8; sleep 0.4
    # Never type unless the cursor really is over there: the keys would land on this Mac.
    if ! state | grep -q "cursor is on"; then echo "  FAIL didn't cross to the other computer; not typing"; exit 1; fi
    if open_sink kbd-sink.sh /tmp/mousetail-kbd; then
      $drive type "hello from the mac 42"; $drive key 24; sleep 0.5
      check "typed text arrived" "$(imac cat /tmp/mousetail-kbd)" "hello from the mac 42"
    else check "test window focused" no yes; fi
    close_sink
    echo "== Command+T becomes Ctrl+T, plain keys unchanged"
    if open_sink raw-sink.sh /tmp/mousetail-raw; then
      $drive key 11 cmd; $drive type a; $drive key 24; sleep 0.5
      check "raw bytes" "$(imac od -An -tx1 /tmp/mousetail-raw | tr -d " \\n")" "14610d"
    else check "test window focused" no yes; fi
    close_sink
    $drive push 3000 0 1; sleep 0.2
    ;;
  clipboard)
    echo "== clipboard follows the cursor"
    token="mousetail-$RANDOM"
    printf 'mac says %s ✓' "$token" | pbcopy
    $drive warp 5 500; sleep 0.2; $drive push -10 0 2; sleep 0.8
    check "Mac clipboard arrived on the iMac" "$(imac wl-paste --no-newline)" "mac says $token ✓"
    imac "printf 'imac says %s' $token | wl-copy >/dev/null 2>&1"
    sleep 0.3
    $drive push 3000 0 1; sleep 0.8
    check "iMac clipboard came back to the Mac" "$(pbpaste)" "imac says $token"
    ;;
  hotkey)
    echo "== Ctrl+Option+Escape brings the cursor home"
    $drive warp 5 500; sleep 0.2; $drive push -10 0 2; sleep 0.3
    check "on the iMac" "$(state | xargs)" "cursor is on 5b9095fb21130aba"
    $drive key 35 ctrl opt; sleep 0.3
    check "home after hotkey" "$(state | xargs)" "cursor is home"
    check "Mac cursor where it left" "$($drive where)" "0, 500"
    ;;
  scroll)
    echo "== trackpad scrolling reaches iMac apps as a trackpad of their own"
    # A window logging its Wayland events, alone on a spare workspace so the cursor is over it.
    ws=$(imac "hyprctl activeworkspace -j | jq .id")
    imac "hyprctl dispatch 'hl.dsp.focus({ workspace = \"9\" })' >/dev/null; rm -f /tmp/mousetail-wl.log; hyprctl dispatch 'hl.dsp.exec_cmd([[sh -c \"WAYLAND_DEBUG=1 foot --app-id=mousetail-test sleep 30 2>/tmp/mousetail-wl.log\"]])' >/dev/null"
    for _ in $(seq 1 20); do sleep 0.25; imac "hyprctl activewindow -j | jq -e '.class == \"mousetail-test\"' >/dev/null" && break; done
    crossings() { grep -c "cursor → " /tmp/mousetail-mac.log; }
    before=$(crossings)
    $drive warp 5 500; sleep 0.2; $drive push -10 0 3; sleep 0.3
    sleep 0.3
    if (( $(crossings) == before )); then echo "  FAIL didn't cross to the other computer ($before, $(crossings))"; exit 1; fi
    # 10 finger moves of 20 points, then the Mac's own momentum (which must not arrive).
    $drive trackpad 20 10; sleep 0.3
    $drive scroll -1; sleep 0.3
    $drive push 3000 0 1; sleep 0.3
    log=$(imac "grep -E 'wl_pointer#[0-9]+\.axis' /tmp/mousetail-wl.log")
    imac "hyprctl clients -j | jq -r '.[] | select(.class == \"mousetail-test\") | .pid' | xargs -r pkill -P; hyprctl dispatch 'hl.dsp.focus({ workspace = \"$ws\" })' >/dev/null"
    finger=$(grep -A1 'axis_source(1)' <<<"$log" | grep -oE 'axis\([0-9]+, 0, [-0-9.]+\)' | grep -oE '[-0-9.]+\)$' | tr -d ')')
    check "finger scroll events" "$(wc -l <<<"$finger" | xargs)" "10"
    check "Omarchy's touchpad scroll factor applied (0.5 x 20)" "$(sort -u <<<"$finger" | xargs)" "10.00000000"
    check "axis stop when the fingers lift" "$(grep -c 'axis_stop' <<<"$log")" "1"
    check "mouse wheel still a wheel" "$(grep -c 'axis_source(0)' <<<"$log")" "1"
    ;;
  freeze)
    echo "== a frozen iMac can't trap the cursor"
    $drive warp 5 500; sleep 0.2; $drive push -10 0 2; sleep 0.3
    check "on the iMac" "$(state | xargs)" "cursor is on 5b9095fb21130aba"
    imac "pkill -STOP -x mousetail"
    start=$(perl -MTime::HiRes=time -e 'printf "%.0f", time*1000')
    while ./target/debug/mousetail status | grep -q "cursor is on"; do
      sleep 0.1
      (( $(perl -MTime::HiRes=time -e 'printf "%.0f", time*1000') - start > 10000 )) && break
    done
    took=$(( $(perl -MTime::HiRes=time -e 'printf "%.0f", time*1000') - start ))
    echo "  (came home after ${took} ms)"
    check "home within 3 s of the iMac freezing" "$(( took <= 3000 ))" "1"
    check "Mac cursor back where it left" "$($drive where)" "0, 500"
    imac "pkill -CONT -x mousetail"
    echo "== reconnects by itself"
    for _ in $(seq 1 40); do sleep 0.5; ./target/debug/mousetail status | grep -q "paired, connected" && break; done
    check "reconnected" "$(./target/debug/mousetail status | grep -c 'paired, connected')" "1"
    ;;
esac
echo "passed $pass, failed $fail"
[[ $fail == 0 ]]
