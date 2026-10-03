# Command Center without SSH: audit and plan

On 2026-10-02 I wrote down: "we need to make sure that the client/host can work fully without ssh, but we can use ssh to diagnose". So I audited every place Command Center used SSH (three sweeps, each one verified), and this is the plan that came out of it.

The audit was done on the Python version of cc-home, before the Rust port. Line numbers in §1 and in the audit at the end point into that version, which is gone now. I've kept them because they show what each change was for.

## Where it stands now

- **The gate is in.** `ssh_argv()` in `crates/cc-home/src/ssh.rs` is the only place that builds an `ssh` or `scp` command line, and without `CC_SSH=1` it stops the feature with "`<feature>` would use SSH; that is diagnostics only (CC_SSH=1). `<fix>`".
- **No automatic fallback.** When a machine isn't paired or its agent doesn't answer, cc-home says `not-paired` or `no-agent` and what to do about it (`crates/cc-home/src/machine.rs`). It never quietly switches to SSH.
- **The test proves it.** `cargo test -p cc-home --test nossh` runs with SSH shimmed out (§4).
- **Still to do:** signing the release (§2), and one leftover hint in the Add form (§5, stage 3).

## Plan

**Goal.** Every user feature works between a Frame and a host over pairing, the agent (TLS on port 3399) and krdp. SSH is only for diagnosing, and only when `CC_SSH=1` is set.

Most of the agent work already existed when I started. The real problem was `agent_for()`: when the agent couldn't be used, it fell back to SSH without telling anyone.

I checked the plan against the Python `home/agent.py` `do()` (lines 397-432) and `host_monitors()` (488), cc-home's `agent_for()` (536-547), `probe()` (1149) and `remote()` (463), and `docs/agent.md` §4-§7.

The agent's `monitors` command already returned, for each shared monitor, its index, output, native width and height, size in mm, x, y, rotation and whether it's primary. That's everything `probe()` and `ready()` were getting from `cc-share list`, `kscreen-doctor -j` and `tagshow.py --info` over SSH, so stage 1 didn't need a new agent command.

## 1. Each shipped SSH use and what replaces it

Line numbers are the Python cc-home's.

| # | SSH use (where) | Replacement |
|---|---|---|
| 1 | `remote()` cc-home:463 | One new gated helper beside it, `ssh_argv(*argv)`. It exits unless `CC_SSH=1`, and every `ssh`/`scp` argv goes through it: `remote()`, `send()`, the scp in `ready()` and the scan `Popen` at 775. No other gate anywhere. |
| 2 | `agent_for()` 536-547, the automatic fallback | Return None only when `CC_SSH=1`. With no host_pk, raise `NotPaired`. If the connect fails, raise `NoAgent(why)`, saying "`<m>`: agent not answering (`<why>`); run `cc-share install` on it, or `CC_SSH=1` to diagnose over SSH". Callers turn these into `@skipped why=not-paired` / `why=no-agent` on the scan channel that already exists. |
| 3 | `ssh_login()` 609-630 | Reached only under `CC_SSH=1`. It reads the `ssh=` option, then the old trusted-hosts `login`, then the agent's `version.login`, and otherwise fails with "set ssh=user@host". |
| 4 | `ready()`'s SSH branch, 724-743 (cc-share list, mkdir/pkill, scp of tagshow.py, `tagshow --info`) | The agent path already covers it: `monitors` (output, mm, size) plus `tags show/hide`. The SSH branch stays as the `CC_SSH=1` path only. |
| 5 | `send()` 491-508, the scp of the tag PNGs | Not used for agent monitors, because the agent draws the tags from parameters. Reached only under `CC_SSH=1`. |
| 6 | The scan's tag screens and host Esc, 774-794 (`ssh … tagshow --serve`) | AgentView and the agent's Esc event, which both exist. The `Popen` is reached only under `CC_SSH=1`. |
| 7 | `probe()` 1149-1178 | Becomes `probe(machine)`: it calls `agent_for(machine).call("monitors")` and maps the result to `(index, output, width, height)`. The old body stays as `probe_ssh()` behind the gate, for diagnosing. |
| 8 | `machine probe <user@host>` 1439 | Takes a paired `<machine>` and goes through the agent. The `user@host` form only works with `CC_SSH=1`. |
| 9 | `machine add` with no WxH, 1444-1453 | A paired machine is probed through the agent. An unpaired one is refused with "give a size (WxH), or Pair it (Pair reads the size)". |
| 10 | The Machines window's Add with an empty Size (`machines.rs` 340-352 `add_args`, 1421) | Change the hint from "or empty to ask the machine" to "WxH; or use Pair to fill it in", and show cc-home's refusal inside the form instead of failing silently. No other logic. |
| 11 | Aligning a hand-added machine (never paired) | The `NotPaired` message from row 2 sends you to Pair. Pairing already upgrades a hand-added line (cc-home 1298-1304). |
| 12 | `session/cc-desktop` 65-70, the password manager tray's "SSH agent" | Rewrite the comment so it's clear no feature depends on it. Keep the tray only if it's wanted for something else. |
| 13 | `login` in the pairing reply (`pair.py:463`) and the `ssh=` written on pairing (cc-home 1244-1304) | Stop sending `login` and stop writing `ssh=`. Existing files are still read under `CC_SSH=1`. This also stops showing the host's account name on the normal pairing path. |
| 14 | `machine unpair` with no agent, 1499-1513 | No SSH here. Change the message to name the exact host step: "on `<host>`: `cc-share unpair <frame>`". |
| 15 | Getting cc-share onto a host and updating it (`cc-share` 47-55, `copy_self`) | The installer in §2. |
| 16 | `docs/agent.md` 197-202 | Becomes true after stages 1-3. Update §5 and the §7 note on the migration fallback: no automatic SSH any more, tell the user to run `cc-share install`. |

Two things needed no change: `tests/clipboard-check`, a manual diagnostic that uses SSH openly, and `selftest-pair`, apart from changing its `ssh_login()` assertion to "pairing writes no `ssh=`/`login`".

## 2. Fresh host: installing without SSH

**I decided on 2026-10-02: a public, signed GitHub release** ("Public signed release"; "let's not do 4, we can just have the installer on github, as it will be public at some point").

I dropped the idea of the Frame serving the installer. Its stub got piped into `sh` without being verified itself, so anyone on the LAN could swap out the checker (review D-052 S1).

The design:

1. **The asset:** `cc-share-bundle-<version>.tar.gz` (`cc-share`, `home/{pair,agent,tagshow}.py`, `third_party/`), built **reproducibly**: sorted entries, fixed mtimes, owner and mode (P6). Its SHA-256 is published with the release.
2. **The signature:** minisign format (Ed25519 over a BLAKE2b-512 prehash), as `<asset>.minisig`.
3. **The installer:** a short script, `install-cc-share`, with the **public key embedded**. It downloads the asset and its signature, verifies them, and only then extracts and runs `./cc-host install`. A tampered asset fails the check. The first design verified with Python `cryptography` (P3), but hosts don't have Python any more, so the verifier has to be something the host already has (for example `ssh-keygen -Y verify` with an Ed25519 key) or a small verifier shipped beside cc-host.
4. **The key's fingerprint, published separately (P2):** in the README and in the Frame's Machines window ("Set up a new host" shows it), with an instruction to compare it with the one the installer prints. The signature catches a tampered asset, but not a compromised repo or account. It's always the **full** fingerprint, never a prefix, because an attacker can grind a key or a tarball to match a short prefix (review S2). The one-line install command shown to users names the fingerprint to compare.
5. **Key custody (P4):** the secret key lives in a password manager, never in the repo. To rotate it, publish the new key in the README and the Machines window and sign the next release with both keys. If it leaks, revoke it in the README, re-sign the current release with a new key and say so in the release notes.
6. **Updates (P7):** a host updates by running the installer again. The Frame only reports "`<host>` runs agent vN, vM is available" (from the agent's `version`). It never pushes code to a host.
7. **While the repo is private (P5):** nobody else can use the release yet. My own hosts install by copying the cc-host binary (built on the Frame) over by hand or with scp and running `./cc-host install`, which links `cc-share` to itself.

> **Now (2026-10-03):** the one-line `install` script and cc-install (crates/cc-install) replace the bundle. `.github/workflows/release.yml` builds static cc-install and cc-host for x86_64 and aarch64, plus cc-home for aarch64, on a `v*` tag, with a `SHA256SUMS` file. The script and cc-install both check every download against it. That catches a broken or swapped download, but not a compromised release or account, because the checksums come from the same place. The signature in points 2 to 5 is still to do: sign `SHA256SUMS` and embed the public key in `install`.

**What `cc-share install` has to do on a fresh host** (docs/agent.md §4 already lists most of it):

- **Install itself properly.** The Python `copy_self` returned early, so re-running `~/.local/bin/cc-share install` never refreshed the agent, pairing or tag-screen code. cc-host now copies itself to `~/.local/share/control-center` and links `~/.local/bin/cc-share` to it, swapping the link in with a rename (`crates/cc-host/src/share.rs`).
- **Check dependencies, but don't install them.** It checks krdpserver and kscreen-doctor (and avahi-publish in the self-check) and prints the package to install when one is missing. The Python checks (python3, `cryptography`, PySide6) went away with Python.
- **No `openssl`.** The krdp certificate is generated in Rust with rcgen (`crates/cc-host/src/hostcert.rs`).
- **Install and enable the units:** share@, guard, agent and announce. Announcing is asked during install. Without it, Discover finds nothing and you have to type the address.
- **Firewall:** open 3399-3449, as before (`--firewall`, with a visible sudo prompt).
- **Keys:** create host-id, host-key and host-cert if they're missing, and never replace them.
- **Self-check:** agent TLS on 3399, kscreen readable, the krdp certificate present, announce running.
- **Finish with pairing.** The last thing it says is to run `cc-share pair` and enter the code on the Frame. From there, discover, pair, connect, align, refit, the camera, the guard, Esc and unpair all run over avahi, pairing, the agent and krdp.

## 3. The single gate

- **The gate is `ssh_argv()` in cc-home**, the only place allowed to build an argv that starts with `ssh` or `scp`. Without `CC_SSH=1` it stops with "`<feature>` would use SSH; that is diagnostics only (CC_SSH=1). `<fix>`", where the caller supplies the fix: "Pair it", "run cc-share install on `<host>`" or "give WxH".
- **`agent_for()` follows the gate.** It doesn't fall back on its own any more (row 2). Under `CC_SSH=1` it prints one line, "CC_SSH=1: `<feature>` over SSH (diagnostics)", so a dev session can always see that SSH is in use.
- **cc-panels has no SSH logic.** It only shows the text it gets from cc-home's stderr and the `@skipped why=` lines.

## 4. Enforcement test: cc-home's `tests/nossh.rs`

This started as a shell script, `tests/no-ssh`, and is a cargo test now: `cargo test -p cc-home --test nossh`.

1. **Shims.** It makes `ssh` and `scp` shims that append to an `ssh.log`, print `SSH CALLED: …` to stderr and exit 97. They go on PATH **and** over /usr/bin/ssh and /usr/bin/scp in a private mount namespace (S8), so an absolute path can't slip past. `CC_SSH` is unset, HOME is a throwaway one (a fresh Frame) and `CC_PANELS_SOCKET` is private, so nothing touches the real files or the running Desktop.
2. **The run.** cc-home's selftest and its SSH-only paths run with the shims in the way.
3. **Assertions.** `ssh.log` stays empty, and every refusal names `CC_SSH=1`.
4. **The gate opens.** The same thing re-runs with `CC_SSH=1`, and `ssh.log` must fill up. That proves the shims really were on the path.
5. **Static check (S7).** The code is scanned for any ssh or scp command line made outside `ssh_argv()`: a quoted command word, an absolute path, a shell call or Rust's `Command::new`. A separate test checks that the scanner catches what it should. `tests/clipboard-check`, a manual diagnostic, isn't scanned.

It runs on the dev box with no cc-panels, no OpenVR and no hardware, so it stays out of the SteamVR binding-load budget.

## 5. Order of work (each stage ships on its own)

1. **Gate and messages**: **done**. `ssh_argv()`; `agent_for()` raises NotPaired/NoAgent (`@skipped why=not-paired|no-agent`); unpair names the host step and keeps a pending unpair while the host still trusts the Frame (S9).
2. **Probe through the agent**: **done**. `probe(machine)` uses `monitors`; `machine probe`/`add` work as in rows 7-9.
3. **Add form**: **half done.** The form shows "give a size (WxH), or Pair it (Pair reads the size)" for an unsized, unpaired machine (`crates/cc-panels/src/control/machines.rs`), but the Size field's error still says "or empty to ask the machine".
4. **Stop the login plumbing**: **done**. No `login` in the pairing reply, no `ssh=` written, selftest-pair asserts it, and the tray comment says no feature depends on it.
5. **Enforcement**: **done**, as in §4.
6. **Installer**: **done in cc-host**, as listed under §2: it installs and links itself, makes the certificate without openssl, asks about announcing and has a self-check (`cc-share check`).
7. **Delivery**: the GitHub release with checksums is in; signing it (§2) is still to do.
8. **Docs**: `docs/agent.md` §5 and §7, the README's host setup, a D-entry recording "SSH = `CC_SSH=1` diagnostics only", .
9. **Later, separately:** starting and stopping sessions through the agent (stage 2 in docs/agent.md).

**Slot 0 (S10):** the shared login stays, and stays visible ("shared login still active" in `cc-share check`, `cc-share frames` and the Machines window), until the user sets a date to retire it.

**Open questions:**

- **Who owns the session code?** I assumed the host-side work owns it.
- **The shared login.** Should a hand-added, unpaired machine keep connecting with the global slot-0 password? It doesn't involve SSH, but it's the one path that gets around pairing.

## Verified SSH uses (the 2026-10-02 audit)

This is what the audit found in the Python version. Paths and line numbers are from then; the files are gone now.

- **cc-home:463-466 `remote()`** (shared helper for align and machine probe/add; shipped; no gate of its own, each caller decides). The generic SSH helper: `ssh -o BatchMode=yes <user@host> bash -s` with the script on stdin. Its callers were align's SSH branch of `ready()` (726, 733, 736) and `probe()` (1152).
- **cc-home:536-547 `agent_for()`** (align and unpair; shipped; `CC_SSH=1` forced SSH, but it also fell back on its own). It returned None, which sent the caller to SSH, in three cases: `CC_SSH=1`; no host_pk in trusted-hosts (never paired, or added by hand); or the connect raising Refused/OSError/ValueError, which printed "no agent (...); over SSH instead". So SSH was an automatic fallback, not an opt-in, which contradicted docs/agent.md:202 ("fall back to SSH only with CC_SSH=1").
- **cc-home:609-630 `ssh_login()`, called at 724** (align; only on the `agent_for()` == None path). It worked out user@host from the `ssh=` option, the target's user if it wasn't cc-*, the trusted-hosts `login`, or another monitor's non-cc login on the same host. With none of those it exited with "no login for <host> to align with", so a paired host with its agent down failed align with an SSH-login error instead of "agent missing". Its comment said "ponytail: SSH is dev-only (D-014)".
- **cc-home:724-743, `ready()`'s SSH branch** (align and the camera scan getting ready; the fallback path). In order: remote `cc-share list` to find monitor N's output; a remote mkdir of ~/.cache/control-center/scan and a pkill of tagshow.py; an scp of home/tagshow.py to the host; then remote `python3 tagshow.py <output> --info` for mm and geometry, which needed PySide6 on the host (otherwise why=no-pyside6).
- **cc-home:491-508 `send()`** (align's tag images; only for monitors without an agent). `ssh mkdir -p .cache/control-center/scan/<name>`, then `scp -q` of the scan PNGs, reporting why=ssh on failure. It returned early for agent monitors, because the agent draws the tags itself.
- **cc-home:774-777 and `listen()` 785-794** (align's tag screens and host Esc; fallback for monitors without an agent). A long-lived Popen per non-agent monitor of `ssh -o BatchMode=yes host bash -c '<SESSION>; exec python3 ~/.cache/control-center/scan/tagshow.py <output> --serve'`, driven over stdin, with a host-side Esc coming back as "escaped" on stdout. Agent monitors used AgentView instead.
- **cc-home:1149-1178 `probe()`** (machine probe and the manual Add; always SSH, with no agent attempt and no `CC_SSH` check). `remote(user_host, 'cc-share list; echo @@; kscreen-doctor -j')` for the shared monitors' outputs and native sizes, even though docs/agent.md:197 said probe used `monitors`.
- **cc-home:1439-1443 `machine probe <user@host>`** (CLI; always SSH). Its comment said it was for the Machines window's Add, but cc-panels never called it directly, only through `machine add`.
- **cc-home:1444-1453 `machine add` without WxH** (always SSH when the size was left out). With no size it called `probe(host)` over SSH and exited if that failed.
- **crates/cc-panels/src/control/machines.rs:340-352 `add_args()`, 1421 `Hit::Add`** (the Machines window's Add machine). An empty Size ran `cc-home machine add <name> <user@host:port>` and so an SSH probe, because the hint said "or empty to ask the machine". A machine added that way was never paired, so its align always went over SSH and its connect used the global slot-0 password.
- **session/cc-desktop:65-70** (the Desktop session). It autostarted a password manager tray with a comment saying "its SSH agent is how cc-home and the scan reach the machines". Not an SSH call itself, but a session-level dependency on an SSH key agent. One sweep wrongly listed this file as SSH-free.
- **home/pair.py:463; cc-home:1244-1246, 1271, 1298, 1304** (pairing, feeding the SSH align fallback; data only). The host's pairing reply sent `'login': getpass.getuser()` "for the Frame's align over SSH (dev-only, D-014)". The Frame saved it in trusted-hosts/<id>.json, wrote `ssh=<login>@<addr>` on new paired monitor lines, and kept a hand-added line's old non-cc user as `ssh=`. It only existed for the SSH fallback, and it exposed the host's account name on the normal pairing path.
- **cc-home:178 `viewers()`, 1066 `OPTIONS`** (align config; data only). Parsed and documented the `ssh=` option ("the host's login, for align"), used only by `ssh_login()`.
- **cc-home:1499-1513 `machine unpair`** (no SSH). It called the agent's unpair through `agent_for()`. With no agent (or `CC_SSH=1`) it only deleted the local trusted-hosts and passwords files, and if the agent call failed it printed "run cc-share unpair <frame> on it". Without an agent, the host kept the Frame's krdp login and frame@ units until someone dealt with it there.
- **cc-share:47-55 `copy_self`, 125-219 `install`** (setup and agent updates; no SSH, but nothing delivered cc-share to a host). `copy_self` returned early when there was no home/pair.py beside the script, so re-running ~/.local/bin/cc-share install never refreshed agent.py, pair.py or tagshow.py. In practice that took a repo checkout on the host, which meant scp or SSH. The install checked krdpserver, jq and kscreen-doctor, but not openssl, which the certificate step needed.
- **docs/agent.md:197-202** (docs ahead of the code). It said probe used the agent's `monitors`, that `ready()` dropped scp/`ssh=` and that every fallback needed `CC_SSH=1`. In the code, probe was SSH-only (cc-home:1152) and `agent_for` fell back to SSH on its own (539-547).
- **tests/clipboard-check:13, 20** (manual test, not shipped). SSHes to two machines to check the shared clipboard. That's an acceptable diagnostic use.
- **cc-home:1765-1849 `selftest-pair`** (selftest, not shipped). Asserted that `ssh_login()` returned the host's login after pairing on localhost. It opened no SSH connection, but needed updating once the login and `ssh=` plumbing went.
- **Verified SSH-free:** cc-home discover 1125-1146 (avahi-browse); pair 1181-1316 and home/pair.py (its own encrypted channel); refit and the camera (scan.py in cc-box); autoconnect (nmcli and local config); rename (local files); cc-share's guard 232-280 (a host systemd unit); cc-share announce, firewall and pair (host-local); home/agent.py (TLS 3399); cc-panels' RDP connect (passwords/<machine> or the global password, plus a TOFU certificate); crates/** and panels/* (no ssh/scp); install.sh; session/cc-launch; cc-view; cc-box. Connect only worked because the share@ and frame@ krdp units were always on (starting sessions through the agent, stage 2, isn't built). Discover needed `cc-share announce on` on the host. The Machines window ran cc-home locally or through distrobox-host-exec, so whether SSH happened was decided inside cc-home: Align (the fallback) and Add with an empty Size.
