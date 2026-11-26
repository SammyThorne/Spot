#!/usr/bin/env bash
# Start an isolated headless GNOME Shell 48 session for automated UI experiments.
#
#   session.sh start <statedir> [WxH ...]   start shell with one virtual monitor per WxH
#   session.sh env   <statedir>             print `export ...` line for clients
#
# Isolation: every XDG_* dir is exported *before* dbus-run-session, so the private
# dbus-daemon and everything it activates (dconf-service, gvfs, portals) inherit the
# isolated paths. GSettings additionally uses the keyfile backend, and a guard checks
# that the real ~/.config/dconf/user is byte-identical before and after (see stop.sh).
#
# Env knobs: CENTER_NEW_WINDOWS=true|false, EXTRA_EXTENSIONS="dir1 dir2" (enabled too),
#            ENABLE_EXTENSIONS="uuid ..." (enable already-installed ones, e.g. via XDG_DATA_DIRS),
#            WITH_GSD=1 (start gsd-media-keys for custom shortcuts), WITH_PORTAL=1
set -euo pipefail
cmd=$1; S=$(realpath -m "$2"); shift 2
here=$(cd "$(dirname "$0")" && pwd)
if [[ $cmd == env ]]; then cat "$S/env.sh"; exit; fi

REAL_DCONF=$HOME/.config/dconf/user
R=/run/user/$(id -u)/spot-$(basename "$S")   # wayland socket path must be <108 bytes
rm -rf "$S" "$R"
mkdir -p "$S"/{config,data/gnome-shell/extensions,data/applications,cache,logs} "$R"
chmod 700 "$R"; ln -s "$R" "$S/run"
sha256sum "$REAL_DCONF" 2>/dev/null | cut -d' ' -f1 > "$S/real-dconf.sha256" || true

uuids=("probe@spot.local")
cp -r "$here/probe@spot.local" "$S/data/gnome-shell/extensions/"
for e in ${EXTRA_EXTENSIONS:-}; do
  cp -r "$e" "$S/data/gnome-shell/extensions/"; uuids+=("$(basename "$e")")
done
for u in ${ENABLE_EXTENSIONS:-}; do uuids+=("$u"); done
ext_list=$(printf "'%s'," "${uuids[@]}"); ext_list="[${ext_list%,}]"

# Pre-seed settings in the isolated keyfile so the shell sees them at startup.
mkdir -p "$S/config/glib-2.0/settings"
cat > "$S/config/glib-2.0/settings/keyfile" <<KF
[org/gnome/shell]
disable-user-extensions=false
enabled-extensions=$ext_list
welcome-dialog-last-shown-version='999'

[org/gnome/mutter]
center-new-windows=${CENTER_NEW_WINDOWS:-false}

[org/gnome/desktop/interface]
enable-animations=false
KF

mons=(); for m in "${@:-1920x1080}"; do mons+=(--virtual-monitor "$m"); done

cat > "$S/inner.sh" <<INNER
#!/usr/bin/env bash
echo "export DBUS_SESSION_BUS_ADDRESS='\$DBUS_SESSION_BUS_ADDRESS' XDG_RUNTIME_DIR=$R XDG_CONFIG_HOME=$S/config XDG_DATA_HOME=$S/data XDG_CACHE_HOME=$S/cache GSETTINGS_BACKEND=keyfile GIO_USE_VFS=local NO_AT_BRIDGE=1 WAYLAND_DISPLAY=spike-0 GDK_BACKEND=wayland XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME; unset DISPLAY" > $S/env.sh
gnome-shell --headless --wayland --no-x11 --wayland-display=spike-0 ${mons[*]} >$S/logs/shell.log 2>&1 &
echo \$! > $S/shell.pid
wait
INNER
chmod +x "$S/inner.sh"

# Exported BEFORE dbus-run-session => inherited by the bus and all activated services.
export XDG_RUNTIME_DIR=$R XDG_CONFIG_HOME=$S/config XDG_DATA_HOME=$S/data XDG_CACHE_HOME=$S/cache
export GSETTINGS_BACKEND=keyfile GIO_USE_VFS=local NO_AT_BRIDGE=1
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME
unset DISPLAY WAYLAND_DISPLAY GNOME_SETUP_DISPLAY DBUS_SESSION_BUS_ADDRESS
setsid dbus-run-session -- "$S/inner.sh" >"$S/logs/session.log" 2>&1 < /dev/null &
echo $! > "$S/session.pid"

for i in $(seq 1 150); do
  if [[ -f $S/env.sh ]]; then
    source "$S/env.sh"
    if gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
         --method org.gnome.Shell.Eval '1' 2>/dev/null | grep -q true; then
      echo "shell ready (unsafe mode) after ~$((i*100))ms"; break
    fi
  fi
  sleep 0.1
done
gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
  --method org.gnome.Shell.Eval '1' 2>/dev/null | grep -q true || {
  echo "shell did not become ready"; tail -20 "$S/logs/shell.log"; exit 1; }

if [[ ${WITH_GSD:-0} == 1 ]]; then
  ( source "$S/env.sh"; exec /usr/libexec/gsd-media-keys ) >"$S/logs/gsd-media-keys.log" 2>&1 &
  echo $! > "$S/gsd.pid"
fi
if [[ ${WITH_PORTAL:-0} == 1 ]]; then
  # portal backends are Wayland clients: they need the full client env (WAYLAND_DISPLAY etc.)
  ( source "$S/env.sh"; exec /usr/libexec/xdg-desktop-portal-gnome ) >"$S/logs/xdp-gnome.log" 2>&1 &
  sleep 0.5
  ( source "$S/env.sh"; exec /usr/libexec/xdg-desktop-portal -r ) >"$S/logs/xdp.log" 2>&1 &
fi
sleep 0.3
