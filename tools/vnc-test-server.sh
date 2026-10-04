#!/bin/bash
# A VNC host to test VNC panels against without any machine: TigerVNC's Xvnc with a test
# pattern (a blue background, xev's window at 100,100 logging clicks and keys, and an xterm), on
# 127.0.0.1:5901, with VeNCrypt X509 (a self-signed certificate made here) and a VNC password.
# Run it inside the control-center container (it needs tigervnc-server-minimal, xterm, xsetroot
# and xev from dnf), where cc-panels can reach it on the loopback.
#   vnc-test-server.sh start   starts it and prints the viewers.conf line for it
#   vnc-test-server.sh stop
#   vnc-test-server.sh test    starts it plus a VNC-password-only one on :5902, runs vnc.rs's
#                              test against both (no headset, no panel), and stops them
# VNC_TEST_PASSWORD sets the password (VNC's own is 8 characters at most). Its files are in
# ~/.cache/control-center/vnc-test.
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
dir=$HOME/.cache/control-center/vnc-test
pw=${VNC_TEST_PASSWORD:-cc-test1}
display=:51
export DISPLAY=$display

stop() {
  for f in "$dir"/*.pid; do
    [ -e "$f" ] && kill "$(cat "$f")" 2>/dev/null || true
    rm -f "$f"
  done
}

fingerprint() {
  openssl x509 -in "$dir/cert.pem" -noout -fingerprint -sha256 | cut -d= -f2 | tr -d : | tr A-F a-f
}

# Xvnc <display> <port> <security types>, in the background, with its pid in <display>.pid
xvnc() {
  Xvnc "$1" -rfbport "$2" -interface 127.0.0.1 -geometry 1280x800 -depth 24 -SecurityTypes "$3" \
    -X509Cert "$dir/cert.pem" -X509Key "$dir/key.pem" -rfbauth "$dir/passwd" >"$dir/xvnc$1.log" 2>&1 &
  echo $! >"$dir/xvnc$1.pid"
  for _ in $(seq 50); do
    xsetroot -display "$1" 2>/dev/null && return
    sleep 0.1
  done
  echo "Xvnc $1 didn't start: $dir/xvnc$1.log" >&2
  exit 1
}

start() {
  stop
  mkdir -p "$dir"
  chmod 700 "$dir"
  [ -s "$dir/cert.pem" ] || openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj /CN=cc-vnc-test \
    -keyout "$dir/key.pem" -out "$dir/cert.pem" 2>/dev/null
  printf '%s\n' "$pw" | vncpasswd -f >"$dir/passwd"
  chmod 600 "$dir/passwd" "$dir/key.pem"
  xvnc $display 5901 X509Vnc
  xsetroot -solid '#2080c0'
  xev -geometry 200x200+100+100 -event button -event keyboard >"$dir/xev.log" 2>&1 &
  echo $! >"$dir/xev.pid"
  xterm -geometry 80x24+400+300 -e bash -c "echo 'Command Center VNC test host'; exec bash" &
  echo $! >"$dir/xterm.pid"
  sleep 0.5
}

case ${1:-start} in
  start)
    start
    fp=$(fingerprint)
    cat <<EOF
Xvnc on 127.0.0.1:5901 (DISPLAY $display), VeNCrypt X509 + VNC password, certificate sha256 $fp
For a VNC panel, put the password where cc-panels finds it, and this line in viewers.conf:
  mkdir -p ~/.config/control-center/passwords && printf '%s' '$pw' > ~/.config/control-center/passwords/vnc-test
  vnc-test  cc@127.0.0.1:5901  9  1280x800  proto=vnc  machine=vnc-test  pin=$fp
or: cc-home machine add vnc-test cc@127.0.0.1:5901 1280x800 proto=vnc machine=vnc-test pin=$fp
EOF
    ;;
  stop) stop ;;
  test)
    start
    trap stop EXIT
    xvnc :52 5902 VncAuth
    cd "$here"
    CC_VNC_TEST="127.0.0.1:5901:$pw:$(fingerprint)" CC_VNC_TEST_PLAIN=127.0.0.1:5902 CC_VNC_TEST_XEV="$dir/xev.log" \
      LD_LIBRARY_PATH=${FREERDP_PREFIX:-$here/panels/third_party/prefix}/lib64 \
      cargo test --release -p cc-panels vnc:: -- --include-ignored --nocapture --test-threads 1
    ;;
  *) echo "usage: vnc-test-server.sh [start|stop|test]" >&2; exit 2 ;;
esac
