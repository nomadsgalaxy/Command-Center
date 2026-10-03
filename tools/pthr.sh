#!/bin/bash
# CPU and wakes per thread: tools/pthr.sh <pid> <seconds> (tools/README.md). Reads /proc only.
P=$1 D=${2:-30} HZ=$(getconf CLK_TCK)
snap() {  # tid cpu-ticks ctxt-switches
  for t in /proc/"$P"/task/*; do
    st=$(< "$t/stat") 2>/dev/null || continue
    set -- ${st##*) }  # $1 = state (field 3): utime ${12}, stime ${13}
    echo "${t##*/} $(( ${12} + ${13} )) $(awk '/ctxt_switches/ {s += $2} END {print s}' "$t/status")"
  done
}
a=$(snap); t0=${EPOCHREALTIME/./}; sleep "$D"; b=$(snap); t1=${EPOCHREALTIME/./}
while read -r tid c w; do
  t=/proc/$P/task/$tid
  sc=$(cut -d' ' -f1 "$t/syscall" 2>/dev/null)
  echo "$tid $c $w $(cat "$t/wchan" 2>/dev/null || echo '?') ${sc:-?} $(cat "$t/comm" 2>/dev/null || echo '?')"
done <<< "$b" | awk -v hz="$HZ" -v dt="$(( t1 - t0 ))" -v pid="$P" -v before="$a" '
  BEGIN { dt /= 1e6; n = split(before, l, "\n"); for (i = 1; i <= n; i++) { split(l[i], f, " "); c0[f[1]] = f[2]; w0[f[1]] = f[3] } }
  ($1 in c0) { cpu = ($2 - c0[$1]) / hz / dt * 100; wk = ($3 - w0[$1]) / dt; tc += cpu; tw += wk; comm = $6; for (i = 7; i <= NF; i++) comm = comm " " $i
               if (cpu > 0.05 || wk > 1) rows[++r] = sprintf("%6.2f%% %6.0f/s tid=%s %-16s wchan=%s sys=%s", cpu, wk, $1, comm, $4, $5); key[r] = cpu }
  END { printf "pid %s total %.1f%% wakes %.0f/s over %.1fs\n", pid, tc, tw, dt
        for (i = 1; i <= r; i++) for (j = i + 1; j <= r; j++) if (key[j] > key[i]) { t = key[i]; key[i] = key[j]; key[j] = t; t = rows[i]; rows[i] = rows[j]; rows[j] = t }
        for (i = 1; i <= r; i++) print rows[i] }'
