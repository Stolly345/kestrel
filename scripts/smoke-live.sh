#!/usr/bin/env bash
# Live-update + permission smoke test. Run as yourself; only the daemon gets sudo.
set -u
[ "$(id -u)" = 0 ] && { echo "run as yourself, not with sudo: the permission checks query as you (it sudoes the daemon itself)"; exit 1; }
cd "$(dirname "$0")/.."
B=$PWD/target/release
export KESTREL_SOCK=/tmp/kestrel-smoke.sock
sudo -v || exit 1
# its own snapshot file: never load or overwrite the installed service's /var/lib/kestrel/index.bin
sudo rm -rf /tmp/kestrel-smoke-state
# leftovers of an earlier run (root-owned if it ran under sudo): a stale log would make the wait
# below see the old run's "indexed" and test against a daemon that never started
sudo rm -f /tmp/kestrel-smoke.log /tmp/kestrel-smoke.sock
# KESTREL_TEST_LOSE: events for paths containing "lose-me" are treated as lost, to exercise the
# repair rescan (KESTREL_LOST_GAP_SECS shortens its 10-minute rate limit)
sudo env KESTREL_SOCK=$KESTREL_SOCK KESTREL_STATE=/tmp/kestrel-smoke-state/index.bin KESTREL_TEST_LOSE=lose-me KESTREL_LOST_GAP_SECS=5 "$B/kestrel-indexd" 2>/tmp/kestrel-smoke.log &
PID=$!
trap 'sudo kill $PID 2>/dev/null; sleep 1; sudo rm -rf /tmp/kestrel-smoke-state; rm -rf "$T"' EXIT
# a root scan can take a while (longer if the installed service is rescanning at the same time)
echo "waiting for the test daemon's first scan…"
# since 0.4 the socket answers right away ("still indexing"); wait for the first scan to finish
for _ in $(seq 360); do
  grep -q indexed /tmp/kestrel-smoke.log 2>/dev/null && break
  [ -d /proc/$PID ] || { cat /tmp/kestrel-smoke.log; echo "FAIL test daemon exited before its first scan"; exit 1; }
  sleep 0.5
done
cat /tmp/kestrel-smoke.log
grep -q indexed /tmp/kestrel-smoke.log || { echo "FAIL first scan didn't finish within 3 minutes"; exit 1; }
T=$HOME/kestrel-smoke-$$
fails=0
check() { # desc query expected-count
  sleep 0.5
  # ks exits 2 when it can't reach the daemon; that must fail, not count as "0 results"
  out=$("$B/ks" "$2"); rc=$?
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
root=$(sudo env KESTREL_SOCK=$KESTREL_SOCK "$B/ks" -n 1 under:/root | wc -l)
[ "$root" = 1 ] && echo "ok   root sees /root" || { echo "FAIL root sees /root"; fails=$((fails+1)); }
check "timeshift excluded" "under:/timeshift" 0
check "docker excluded" "under:/var/lib/docker" 0

# content: reads files as the asker (the daemon switches its fsuid to yours)
echo "kestrel-smoke-mine-$$" > "$T/mine.txt"
check "content: finds my own file" "under:$T mine.txt content:kestrel-smoke-mine-$$" 1

# a lost event: missing at first, then found by the repair rescan (gap 5 s + a ~10-20 s scan).
# Also lost: a root-owned world-readable file, used below to show the kernel enforcing
# permissions when the index is stale. Created together, so one repair rescan covers both.
SECRET=$HOME/kestrel-smoke-lose-me-secret.txt
sudo sh -c "echo kestrel-smoke-secret-$$ > '$SECRET' && chmod 644 '$SECRET'"
touch "$HOME/kestrel-smoke-lose-me.txt"
check "lost event: not applied" "kestrel-smoke-lose-me" 0
# the first lost event starts a rescan at once, which may pass $HOME before the second file
# exists; that one then waits out the 5 s gap for the next rescan. So wait for the answer itself.
for _ in $(seq 120); do [ "$("$B/ks" kestrel-smoke-lose-me | grep -c .)" -ge 2 ] && break; sleep 1; done
check "lost event: repaired by rescan" "kestrel-smoke-lose-me.txt" 1
rm -f "$HOME/kestrel-smoke-lose-me.txt"
check "content: root's readable file" "under:$HOME kestrel-smoke-lose-me-secret content:kestrel-smoke-secret-$$" 1
sudo chmod 600 "$SECRET"
check "content: kernel says no (stale index)" "under:$HOME kestrel-smoke-lose-me-secret content:kestrel-smoke-secret-$$" 0
sudo rm -f "$SECRET"
# the chmod and rm were lost events too: let their repair rescan finish before the drive checks
for _ in $(seq 120); do [ "$("$B/ks" kestrel-smoke-lose-me | grep -c .)" -eq 0 ] && break; sleep 1; done

# a "USB drive": a small ext4 image mounted while the daemon runs, then unmounted
IMG=/tmp/kestrel-smoke.img; MNT=/tmp/kestrel-smoke-drive
truncate -s 64M "$IMG" && mkfs.ext4 -q -F "$IMG"
sudo mkdir -p "$MNT" && sudo mount -o loop "$IMG" "$MNT" && sudo touch "$MNT/usb-only-file.txt"
sleep 2
check "drive mounted: indexed" "usb-only-file" 1
if sudo umount "$MNT"; then echo "ok   drive unmounts (daemon doesn't hold it busy)"; else echo "FAIL umount: drive busy"; fails=$((fails+1)); fi
sleep 1
check "drive unmounted: dropped" "usb-only-file" 0
sudo rmdir "$MNT"; rm -f "$IMG"

echo "$fails failure(s)"; exit $fails
