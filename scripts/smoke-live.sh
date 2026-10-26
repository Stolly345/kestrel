#!/usr/bin/env bash
# Live-update + permission smoke test. Run as yourself; only the daemon gets sudo.
set -u
cd "$(dirname "$0")/.."
B=$PWD/target/release
export CHIMERA_SOCK=/tmp/chimera-smoke.sock
sudo -v || exit 1
# its own snapshot file: never load or overwrite the installed service's /var/lib/chimera/index.bin
sudo rm -rf /tmp/chimera-smoke-state
# CHIMERA_TEST_LOSE: events for paths containing "lose-me" are treated as lost, to exercise the
# repair rescan (CHIMERA_LOST_GAP_SECS shortens its 10-minute rate limit)
sudo env CHIMERA_SOCK=$CHIMERA_SOCK CHIMERA_STATE=/tmp/chimera-smoke-state/index.bin CHIMERA_TEST_LOSE=lose-me CHIMERA_LOST_GAP_SECS=5 "$B/chimera-indexd" 2>/tmp/chimera-smoke.log &
PID=$!
trap 'sudo kill $PID 2>/dev/null; sleep 1; sudo rm -rf /tmp/chimera-smoke-state; rm -rf "$T"' EXIT
# a root scan can take a while (longer if the installed service is rescanning at the same time)
echo "waiting for the test daemon's first scan…"
# since 0.4 the socket answers right away ("still indexing"); wait for the first scan to finish
for _ in $(seq 360); do grep -q indexed /tmp/chimera-smoke.log 2>/dev/null && break; sleep 0.5; done
cat /tmp/chimera-smoke.log
grep -q indexed /tmp/chimera-smoke.log || { echo "FAIL first scan didn't finish within 3 minutes"; exit 1; }
T=$HOME/chimera-smoke-$$
fails=0
check() { # desc query expected-count
  sleep 0.5
  # cx exits 2 when it can't reach the daemon; that must fail, not count as "0 results"
  out=$("$B/cx" "$2"); rc=$?
  if [ $rc -eq 2 ]; then echo "FAIL $1: daemon unreachable"; fails=$((fails+1)); return; fi
  got=$(printf '%s' "$out" | grep -c .)
  if [ "$got" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: '$2' gave $got, want $3"; fails=$((fails+1)); fi
}
mkdir -p "$T/a"; touch "$T/a/alpha.txt"; check "create"            "$T/a/alpha.txt" 1
mv "$T/a" "$T/b";                       check "dir rename (new)"  "$T/b/alpha.txt" 1
                                        check "dir rename (old)"  "$T/a/alpha.txt" 0
echo hi > "$T/b/alpha.txt";             check "modify"            "$T/b/alpha.txt size:3" 1
mkdir -p "$T/x/y/z"; touch "$T/x/y/z/deep.rs"; check "new tree"   "$T/x/y/z/deep.rs" 1
rm -r "$T/b";                           check "delete"            "$T/b/alpha.txt" 0
check "no /root for you" "under:/root" 0
root=$(sudo env CHIMERA_SOCK=$CHIMERA_SOCK "$B/cx" -n 1 under:/root | wc -l)
[ "$root" = 1 ] && echo "ok   root sees /root" || { echo "FAIL root sees /root"; fails=$((fails+1)); }
check "timeshift excluded" "under:/timeshift" 0
check "docker excluded" "under:/var/lib/docker" 0

# a lost event: missing at first, then found by the repair rescan (gap 5 s + a ~10-20 s scan)
touch "$HOME/chimera-smoke-lose-me.txt"
check "lost event: not applied" "chimera-smoke-lose-me" 0
for _ in $(seq 90); do grep -q 'events were lost' /tmp/chimera-smoke.log && break; sleep 1; done
for _ in $(seq 90); do [ "$(grep -c indexed /tmp/chimera-smoke.log)" -ge 2 ] && break; sleep 1; done
check "lost event: repaired by rescan" "chimera-smoke-lose-me" 1
rm -f "$HOME/chimera-smoke-lose-me.txt"

# a "USB drive": a small ext4 image mounted while the daemon runs, then unmounted
IMG=/tmp/chimera-smoke.img; MNT=/tmp/chimera-smoke-drive
truncate -s 64M "$IMG" && mkfs.ext4 -q -F "$IMG"
sudo mkdir -p "$MNT" && sudo mount -o loop "$IMG" "$MNT" && sudo touch "$MNT/usb-only-file.txt"
sleep 2
check "drive mounted: indexed" "usb-only-file" 1
if sudo umount "$MNT"; then echo "ok   drive unmounts (daemon doesn't hold it busy)"; else echo "FAIL umount: drive busy"; fails=$((fails+1)); fi
sleep 1
check "drive unmounted: dropped" "usb-only-file" 0
sudo rmdir "$MNT"; rm -f "$IMG"

echo "$fails failure(s)"; exit $fails
