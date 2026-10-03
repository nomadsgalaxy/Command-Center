#!/bin/bash
# How long a process's main loop stops: tools/stall3.sh <pid> <seconds> (tools/README.md). It polls
# /proc every ~2 ms with bash builtins only, so there's no fork per poll, and prints one line per
# gap over 45 ms.
pid=$1 secs=$2 base=/proc/$1/task/$1
exec 3<> <(:)  # a pipe nobody writes: read -t on it is a builtin sleep
vcs() { local k v; while read -r k v _; do [[ $k == voluntary_ctxt_switches: ]] && { V=$v; return; }; done < "$base/status"; }
ss() { local a b; read -r a b _ < "$base/schedstat"; RUN=$a WAIT=$b; }
vcs; last=$V; t_last=${EPOCHREALTIME/./}; ss; run0=$RUN wait0=$WAIT; end=$(( t_last + ${secs%.*} * 1000000 ))
declare -A seen
while now=${EPOCHREALTIME/./}; (( now < end )); do
  read -t 0.002 -u 3 _
  vcs; now=${EPOCHREALTIME/./}
  if (( now - t_last > 30000 && V == last )); then
    st=$(< "$base/stat"); st=${st##*) }; wc=$(< "$base/wchan") 2>/dev/null
    seen["('${st%% *}', '${wc:--}')"]=$(( ${seen["('${st%% *}', '${wc:--}')"]:-0} + 1 ))
  fi
  if (( V != last )); then
    g=$(( now - t_last ))
    if (( g > 45000 )); then
      ss; s=""; for k in "${!seen[@]}"; do s+="${s:+, }$k: ${seen[$k]}"; done
      printf '%(%H:%M:%S)T.%s gap %d ms: on-CPU %d ms, waiting for a CPU %d ms; states seen {%s}\n' -1 "${now:10:3}" $(( g / 1000 )) \
        $(( (RUN - run0) / 1000000 )) $(( (WAIT - wait0) / 1000000 )) "$s"
    fi
    ss; seen=(); last=$V t_last=$now run0=$RUN wait0=$WAIT
  fi
done
