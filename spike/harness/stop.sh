#!/usr/bin/env bash
# Stop an isolated session started by session.sh and verify the real dconf db is untouched.
S=$(realpath -m "$1")
[[ -f $S/session.pid ]] || { echo "no session at $S"; exit 0; }
sid=$(ps -o sid= -p "$(cat "$S/session.pid")" 2>/dev/null | tr -d ' ')
[[ -n $sid ]] && pkill -TERM -s "$sid" 2>/dev/null
for p in gsd.pid; do [[ -f $S/$p ]] && kill "$(cat "$S/$p")" 2>/dev/null; done
sleep 0.5
rm -rf "/run/user/$(id -u)/spot-$(basename "$S")"
before=$(cat "$S/real-dconf.sha256" 2>/dev/null)
after=$(sha256sum "$HOME/.config/dconf/user" 2>/dev/null | cut -d' ' -f1)
if [[ -n $before && $before != "$after" ]]; then
  echo "WARNING: real ~/.config/dconf/user changed during the run (may be your own desktop activity)"; exit 2
fi
echo "stopped; real dconf untouched"
