# Source on the iMac (over SSH) to reach the running Hyprland session.
export XDG_RUNTIME_DIR=/run/user/$(id -u)
export WAYLAND_DISPLAY=$(ls -t "$XDG_RUNTIME_DIR"/wayland-[0-9]* | grep -v '\.lock$' | head -n1 | xargs basename)
export HYPRLAND_INSTANCE_SIGNATURE=$(ls -t "$XDG_RUNTIME_DIR"/hypr | head -n1)
export PATH="$HOME/.cargo/bin:$PATH"
