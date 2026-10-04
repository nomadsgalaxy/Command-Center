# Discovery and pairing

*My design, reviewed twice (D-048: H1, H2, M1–M4, L1–L3 in).*

> **Now:** `cc-host` implements this design (Rust, crates/cc-host; docs/rust-host.md). The Python
> files it names (pair.py, agent.py, tagshow.py, third_party/spake2) were removed on 2026-10-03,
> but the decisions and limits below still hold.

This is how a Frame finds a host on the network and gets what it needs to show that host's
monitors, with no login on the host beyond pairing. My decisions (2026-10-02):
announcing is opt-in per host; pairing uses a key shown on the host's screen and typed on the
Frame; each paired Frame gets its own krdp login; and libraries and the font are vendored in the
repo.

## 1. Announce (host, opt-in)

`cc-share announce on|off|status` controls it, and it's off by default. `on` enables the user unit
`control-center-announce`, which publishes an mDNS service with `avahi-publish`:

- type `_controlcenter._tcp`, port **3399** (the host agent's port; krdp uses 3400+)
- TXT: `host=<hostname>` `monitors=<N>` `m<i>=<output>,<W>x<H>` per shared monitor (native
  pixels, rotated for a portrait output) `version=1` `pair=0|1`. There's no login name, because
  pairing doesn't need it (M2).
- republished when any of it changes (checked every 2 s), with `pair=1` while a pairing screen is
  up

The agent on 3399 listens **only while a pairing screen is up** (section 3), never just because
announcing is on.

## 2. Discover (Frame)

`cc-home machine discover [--wait S]` prints one line per host:

    @host name=<host> addr=<ip> port=<port> monitors=<N> m0=<output>,<W>x<H> ... pair=0|1 known=<viewer,...>|-

It prefers the IPv4 address on the default route's interface. `known` lists viewers.conf entries
already pointing at that host.

## 3. The pairing key (host)

`cc-share pair` opens the key screen. The Machines window asks the host's agent to open it, and
the user can also run it there.

- The key is **6 digits** from a CSPRNG (`secrets.randbelow(10**6)`), shown as two groups:
  `482 917`. It's digits only, so nothing can be confused when it's blurred through passthrough.
- It's shown full screen: white on #0B0B0F, with digits at least 1/4 of the screen height, in
  **Atkinson Hyperlegible Mono** (bundled; B612 Mono as a fallback), plus "Esc to cancel".
- **Single use.** The key is destroyed on success, after **3 failed confirmations**, after
  **5 minutes** (monotonic clock), on **Esc**, or by `cc-share stop`. Only a wrong key counts as a
  failed confirmation: a malformed, oversize or slow message just closes that connection, so a
  stranger can't burn the key with junk (M1). A destroyed key is never reused. Pairing again
  needs a new `cc-share pair` at the host, so someone has to be there.
- Rate limit across keys: a new key can't be opened within 30 s of a lockout.
- While the key is open, the file `~/.config/control-center/pairing` exists (announce sets
  `pair=1`), and the agent listens on TCP 3399. For the firewall, see section 5 (M3).
- The key is on screen, so anyone who can see that screen (a person, or another viewer of that
  monitor) can pair while it's up. That's the trust model: physical presence at the host.

### Reading the key with the camera ("Pair by looking")

The key screen also shows the key as **three ArUco 4x4 tags** (`DICT_4X4_1000`, ids **900–999**,
outside the 250 that align uses). Each tag carries two digits (id − 900), and they're read left to
right along the tags' own x axis, so a portrait monitor or a tilted head reads the same. The bit
patterns are embedded in pair.py (`key_tag_cells`), since hosts have no OpenCV.

The same screen shows the host's **LAN address** as four more tags under the key's, one per
octet, plus the address in plain text. They're `DICT_4X4_1000` ids **300–555** (the octet is id −
300), which is clear of align's 0–249 and the key's 900–999. They're read left to right along the
tags' own x axis, like the key's. The address is the IPv4 source address of the host's default
route (the one the kernel would send from, nothing is actually sent), and only if it's private.
With several routes the kernel's choice (lowest metric) is the one shown. With no route, or a
public address, the screen has no address tags. The bit patterns are in cc-host's `aruco.rs`,
and a test checks them against cc-scan's dictionary.

`cc-home machine pair --scan [addr] [--replace]` pairs with `addr` if it's given. Otherwise, if
exactly one host is announcing `pair=1`, it uses that one. If mDNS is off or blocked on the host
(or finds none, or several), it reads the address off the tags along with the key. It reads the
tags through the VR mirror camera, using cc-panels' HUD ("Look at the pairing key on the host's
screen", green outlines, a cancel button) and head pose. It never connects to SteamVR itself, and
it refuses to run without cc-panels. The same key (and address) read twice in a row counts. The key
goes to pair() inside cc-home and is never printed. Progress shows as
`@pair <addr> host=<name> state=scanning` (an announcing host) or `@pair <addr> state=scanning`
(an address off the tags), `@pairscan state=looking|read|cancelled|timeout|refused`, then the
lines of section 8. The Machines window's Add machine form has the button too, with or without
found hosts, and then runs the scan with no address.

**The address is only where to connect.** The key is still the secret, and the host's pinned key
still comes from the pairing handshake (section 4), so a wrong address can't pair: whoever answers
there has to know the key. Still, the Frame only takes a **private IPv4 address** off a screen
(10/8, 172.16/12, 192.168/16). Anything else is refused (`@pairscan state=refused`), so a screen
can't send the Frame's pairing to the internet. Tailscale's 100.64/10 isn't private in that sense,
so a host reached over it needs its address typed.

**When the host isn't in the list:** it's probably not announcing (mDNS is off, or blocked on its
network). The user doesn't need to fix that. They run `cc-share pair` on the host, open Add machine
on the Frame, and press Pair by looking while facing the host's screen. Typing the host's address
and the 6 digits stays as the fallback.

**Trust:** the tags carry the whole key, so any camera that sees the host's screen can read it,
just as any person who sees it can read the digits. It's the same physical-presence model
(section 3), and nothing about the exchange changes. Typing the digits stays as the fallback.

## 4. The exchange (TCP 3399, newline-delimited JSON)

Limits: one connection at a time; each message at most 4 KiB; a 10 s read timeout per step; and
strict parsing (unknown fields, wrong types or oversize messages drop the connection, but that
doesn't count as a try, per M1: only a wrong key does). Binary values are base64.

1. **F → H** `{"v":1, "frame":"<frame name>", "frame_pk":"<Ed25519 public key>", "spake":"<msg A>"}`

   **The Frame name is untrusted input (H1).** It must match `^[a-z0-9][a-z0-9-]{0,31}$` before
   it's used anywhere (file names, the krdp user `cc-<frame>`, unit names, command lines).
   Otherwise H answers `{"error":"bad-name"}` and closes (not a try). If that name is already
   trusted with a **different** `frame_pk`, H shows "Replace the Frame <name>? Enter: yes,
   Esc: no" on the pairing screen and only goes on after Enter; otherwise it answers
   `{"error":"name-taken"}`.
2. **H → F** `{"v":1, "host":"<hostname>", "host_pk":"<Ed25519 public key>", "spake":"<msg B>"}`

   SPAKE2 (vendored python `spake2`, 0.9) runs as
   `SPAKE2_A(key, idA=b"cc-frame:"+frame, idB=b"cc-host:"+host)` on the Frame and `SPAKE2_B` with
   the same ids on the host, where `key` is the 6 digits as ASCII with spaces removed. Both get K
   (32 bytes). Under a PAKE a 6-digit key resists offline guessing, and the lockout limits online
   guessing to 3 per key.

3. **Transcript** `T` = SHA-256 of the canonical JSON of messages 1 and 2 (sorted keys, no
   spaces), so the version, both names, both public keys and both SPAKE messages are all bound.
4. **Keys** come from K by HKDF-SHA256 (salt = T), each with its own label:
   `cc-pair confirm F`, `cc-pair confirm H`, `cc-pair seal F->H`, `cc-pair seal H->F`.
5. **Confirmation:** F → H `{"confirm": HMAC(kcF, T)}`, and H checks it (constant time). If it's
   wrong, H counts a try, answers `{"error":"bad-key","tries_left":n}` and closes; at 0 the key is
   destroyed (`{"error":"locked"}`). If it's right, H → F `{"confirm": HMAC(kcH, T)}`, and F checks
   it (a host that doesn't know the key can't answer).
6. **Sealed** with ChaCha20-Poly1305 (`cryptography`): one key per direction, the nonce a 96-bit
   counter starting at 0 per direction and never reused, and T as associated data.
   - H → F: `{"user":"<krdp user>", "password":"<krdp password>", "slot":k,
     "cert_sha256":"<hex SHA-256 of the krdp TLS certificate (cert.pem), DER>",
     "monitors":[{"index":i, "output":"DP-1", "width":W, "height":H, "port":3400+10k+i}]}`
   - **Certificate pinning (H2):** F stores `cert_sha256` with the host
     (`trusted-hosts/<host>.json`), and cc-panels checks it on every connect to that machine
     (FreeRDP `CertificateAcceptedFingerprints`, set per machine), refusing any other certificate.
     Without it, the credentials would go to whoever answered at that address later.
   - **Re-pairing a known host:** if `host_pk` differs from the one trusted for that address, F
     stops before sending anything sealed and says so (`@pair <host> state=host-changed`). It only
     goes on with `cc-home machine pair <addr> <key> --replace`.
   - F → H: `{"ok":true}`. H then stores the Frame and starts its krdp servers.

If Esc is pressed on the host at any point, H sends `{"error":"cancelled"}` (if connected),
closes, and destroys the key. F prints `@pair <host> state=cancelled`.

## 5. Credentials (one krdp login per Frame)

`krdpserver` takes one `-u`/`-p` per process, so each paired Frame gets its own servers:

- **slot** k ≥ 1 per Frame (slot 0 is today's shared login, `-u $USER` with
  `~/.config/control-center/password`, kept until the user retires it)
- user `cc-<frame>` (the name validated, H1), and a password of 24 random characters from
  `[A-Za-z0-9]` (safe in a shell and a unit file), stored on the host at
  `~/.config/control-center/frames/<frame>.json` (0600, dir 0700)
- units `control-center-frame@<frame>-<m>.service` (`cc-share frame-run`, which reads the slot and
  login from `frames/<frame>.json`): `krdpserver --monitor m --port 3400+10k+m -u cc-<frame> -p <its password>`.
  Pairing a Frame again keeps its slot and restarts its units with the new password.
- the guard counts connections on every slot's ports, so it doesn't change modes or stop krdp
  while a paired Frame is connected
- never the user's account password
- **At most 4 Frames** (slots 1–4, ports up to 3449). Measured on the desktop (2026-10-02): each
  krdpserver holds ~800 MB resident, and with the Frame connected the two used ~0.4 core together.
  Whether an idle server (no client) captures or encodes is still to be measured before slots
  ship. If it does, a Frame's servers start on demand rather than at login (M4).
- **Firewall (M3):** if firewalld or ufw is active, `cc-share pair` prints the exact commands to
  open 3399–3449 (pairing plus every slot), and `cc-share unpair` of the last Frame prints the ones
  to close them. Either runs them only with `--firewall`, under an interactive sudo prompt the user
  sees. Never a silent sudo.
- **The password is visible (L1)** to the same user in `ps` and in the unit file
  (`krdpserver -p`), as the shared login's is today. That's acceptable on a single-user desktop,
  and a later krdp that reads it from a file or the environment would remove the problem.

## 6. Trust and storage

- **Frame:** an Ed25519 key pair in `~/.config/control-center/frame-key` (private key 0600, dir
  0700), made at the first pairing. Per host, it keeps `trusted-hosts/<host>.json` (0600: `addr`,
  `host_pk`, `cert_sha256`) and the krdp password in `passwords/<machine>` (0600), where the
  machine is the host's name. cc-panels reads `passwords/<machine>`, falling back to the global
  `password` file. These are **plaintext on the Frame**, protected only by file permissions; the
  system keyring is a later option.
- **Host:** `trusted-frames/<frame>.pub` (0600) with the Frame's public key, plus
  `frames/<frame>.json`.
- **What "trusted" means afterwards:** the credentials live only on the Frame. A Frame can't fetch
  them again; pairing again (with a new key at the host) replaces them. The Frame's public key is
  kept so later host-agent features can authenticate the Frame with a signed challenge.
- Nothing logs K, derived keys, private keys or passwords. Only the host's screen shows the 6
  digits.

### Host id and labels (D-050)

- **Host id:** each host makes a random UUID once, at first pairing
  (`~/.config/control-center/host-id`, 0600). It's never derived from its name or MAC, and a
  reinstall keeps the file. It's sent **only in the sealed reply** (`id`), never announced. The
  reply carries no account name.
- **On the Frame,** the id keys everything kept for that host: `machine=<id>` on its viewers.conf
  lines, `trusted-hosts/<id>.json` (`id, host, addr, host_pk, cert_sha256, label, monitors`
  {index: output}) and `passwords/<id>`. Renaming the host or a new address breaks nothing.
- **Re-pairing** finds this machine's line for each monitor by (machine id, monitor index), then
  by name, then by a line added by hand on that host's 3400+m (which keeps its name, options and
  spots). Viewer names, and so spot keys, never change.
- **A changed id** for a known address or host key is `state=host-changed`, like a changed key.
  Only `--replace` trusts it (its lines move to the new id).
- **Migration:** a host paired before ids (files and `machine=` by its name) moves to its id at
  its next pairing, in one step (files renamed, `machine=` rewritten, label kept).
- **Labels:** a machine's label is in `trusted-hosts/<id>.json` `label` (plain string). A
  monitor's own label is `label=` on its line, **percent-encoded** (RFC 3986: everything but
  `A-Za-z0-9-._~` as `%XX` of its UTF-8 bytes, so viewers.conf stays whitespace-split), at most 64
  characters. What's shown is the monitor's label; else the machine's label, plus the output when
  the machine has more than one monitor; else the viewer's name. Set them with
  `cc-home machine rename <machine> <label>` / `rename <monitor> <label> --monitor` (empty
  clears). Names, labels and ids all work wherever cc-home takes a name, and
  `machine list --json` gives name, label, display, machine, machine_label, host, output.

## 7. Revoke

- Host: `cc-share frames` lists the paired Frames. `cc-share unpair <frame>` stops and removes that
  Frame's krdp units and deletes `frames/<frame>.json` and `trusted-frames/<frame>.pub`. Its login
  stops working at once.
- Frame: `cc-home machine unpair <machine>` deletes `trusted-hosts/<host>.json` and
  `passwords/<machine>`. Its viewers.conf lines stay until `cc-home machine remove`.

## 8. Progress lines (for the Machines window)

`cc-home machine pair <addr> <key|-> [--replace]` prints
`@pair <addr> state=waiting|ok|bad-key|expired|cancelled|locked|host-changed|bad-name|name-taken|full|failed|too-many`
and, at the end, `@paired <host> monitors=N cred=per-frame`. With `-`, the key is one line on
stdin, so `ps` doesn't show it (cc-home hands it to pair.py the same way). Each monitor becomes a
viewers.conf line `<host>-<output>` (lower case) with `machine=<host>` and the user `cc-<frame>`.
Pairing again updates those lines' login and port and keeps their options. The Frame's name is its
hostname in lower case, with anything outside `[a-z0-9-]` turned into `-`.

The Frame's typing screen shows the **host's name large**, so the user can match it to the screen
showing the key. The Frame also has its own limit of **3 tries per host per 10 minutes**, so a
fake host can't keep asking for the key (L3).

## 9. Interoperability (D-043: other hosts, the Rust port)

Hosts are Linux only today, because `avahi-publish`, systemd user units and cc-share are Linux
tools. macOS (`dns-sd`, launchd) and Windows (Bonjour / mDNS, a service) hosts need their own
equivalents (L2).

Python `spake2` (Brian Warner) is **not RFC 9382**: its group, M/N points and message format are
its own. The Rust `spake2` crate (also Warner's) was written to match it, but this must be
**tested before** a Rust or non-Python side ships, with a cross-test that uses fixed passwords and
ids and checks that both derive the same K. Until then, both ends are the vendored Python.

## 10. Vendored

- `third_party/spake2/` (MIT, with its LICENSE), pure Python, **pinned**: the exact release and
  upstream commit are recorded in `third_party/spake2/VERSION` when it's vendored. It needs
  `hkdf`, vendored the same way if it isn't in the stdlib path.
- `third_party/fonts/AtkinsonHyperlegibleMono-*.ttf` (SIL OFL 1.1, with its licence).
- `cryptography` is already on the hosts; in the Frame's container, install it with
  `pip install --user cryptography`.
