#!/bin/bash
# ccpower.sh LABEL [SECS=300] - read-only power/duty A/B probe for Command Center.
# Run it unplugged, with no builds or agents, Steam UI video closed, and a fixed backlight and scene.
# It appends one CSV row to ~/ccpower.csv. Compare rows; repeat A B A B, because rail noise is about 0.5 W.
L=${1:?label}; S=${2:-300}; H=/sys/class/hwmon; B=/sys/class/power_supply/max1720x_bat_7-36
out=${CCPOWER_OUT:-$HOME/ccpower.csv}
cg(){ echo /sys/fs/cgroup$(systemctl --user show -p ControlGroup --value "$1"); }
cpu(){ awk '/^usage_usec/{print $2}' "$(cg "$1")/cpu.stat" 2>/dev/null || echo 0; }
# cc-panels runs inside the cc-box podman cgroup, which builds share, so count it by PID instead.
P=$(for p in $(pgrep -x cc-panels); do case $(readlink /proc/$p/exe) in */target/release/cc-panels) echo $p ;; esac; done | head -1)
pcpu(){ awk '{print ($14+$15)*10000}' /proc/$P/stat 2>/dev/null || echo 0; }   # usec, CLK_TCK=100
wk(){ cat /proc/$P/task/*/status 2>/dev/null | awk '/^voluntary_ctxt/{n+=$2}END{print n+0}'; }
p0=$(pcpu); d0=$(cpu cc-desktop); w0=$(wk); t0=$(date +%s)
v=0; a=0; g=0; bw=0; n=0
for ((i=0; i<S/2; i++)); do
  v=$((v+$(cat $H/hwmon53/power1_input)))
  a=$((a+$(cat $H/hwmon51/power1_input)+$(cat $H/hwmon51/power2_input)+$(cat $H/hwmon51/power3_input)))
  g=$((g+$(cat $H/hwmon52/power1_input)))
  bw=$((bw+$(awk -v i=$(cat $B/current_now) -v u=$(cat $B/voltage_now) 'BEGIN{x=i*u/1e6; printf "%d", (x<0?-x:x)}')))
  n=$((n+1)); sleep 2
done
dt=$(( $(date +%s)-t0 ))
awk -v L="$L" -v n=$n -v dt=$dt -v v=$v -v a=$a -v g=$g -v bw=$bw \
    -v pc=$(( $(pcpu)-p0 )) -v dc=$(( $(cpu cc-desktop)-d0 )) -v w=$(( $(wk)-w0 )) \
    -v st="$(cat $B/status)" -v cap=$(cat $B/capacity) -v t=$(cat $H/hwmon4/temp1_input) -v f=$(cat $H/hwmon50/fan1_input) -v out="$out" '
BEGIN{ if (system("test -s " out)) print "date,label,secs,bat_status,cap,vph_W,cpu_W,gfx_W,bat_W,ccpanels_core,ccdesktop_core,ccpanels_wakes_s,cpu_C,fan" > out
  printf "%s,%s,%d,%s,%d,%.2f,%.2f,%.2f,%.2f,%.3f,%.3f,%.0f,%.1f,%d\n", strftime("%F %T"), L, dt, st, cap,
    v/n/1e6, a/n/1e6, g/n/1e6, bw/n/1e6, pc/dt/1e6, dc/dt/1e6, w/dt, t/1000, f >> out }'
tail -1 "$out"
