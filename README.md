<img src="packaging/command-center.png" alt="" width="96">

# Command Center

Command Center is the Steam Frame's VR desktop. It puts the windows of the Frame's own Plasma session and the monitors of your other computers in the room with you as panels, each one where you want it. The Frame's mouse and keyboard drive whichever panel you point at, so one set of hands works every machine. It's all Rust. More at [framecc.nomadsgalaxy.com](https://framecc.nomadsgalaxy.com).

Here's what runs where:

| Piece | Runs on | What it does |
| --- | --- | --- |
| `cc-host` (also called as `cc-share`) | each computer you want to see (Arch, KDE Plasma on Wayland, `krdp`) | Serves monitor N over RDP on port 3400+N with `krdpserver --plasma --monitor N`, as the user service `control-center-share@N`, and runs the agent the Frame pairs with. `cc-share` is a link to the same binary. |
| `cc-panels` | the Frame | The Desktop itself: every remote monitor and every window of its Plasma session as a SteamVR panel, plus the taskbar, pointer and keyboard. |
| `cc-home` | the Frame | Spots, workspaces, machines, pairing and align. Run `cc-home` with no arguments to see its commands. |

## Install

For now the Frame needs a Bluetooth mouse and keyboard paired with it, since that's what drives
the panels.

Run this in a terminal on the Steam Frame (Konsole, or over SSH), and then on each computer you
want to see from it:

```
curl -fsSL https://framecc.nomadsgalaxy.com/install | sh
```

It downloads the installer (`cc-install`) from the latest release, checks it against the
release's `SHA256SUMS`, and works out which machine it's on. Running it again updates
Command Center or removes it.

- **On the Frame**, it gets the code into `~/control-center` and builds it in a container
  (`control-center`, through distrobox). The first run takes a while. When it's done, open
  **Desktop** from the SteamVR launcher. If the pointer driver changed, it tells you: SteamVR only
  loads drivers when it starts, so the installer offers to restart the Frame and waits for your
  yes. A Desktop that's already open keeps running while it builds, and the update loads the next
  time you open it.
- **On a computer** (KDE Plasma on Wayland, with krdp), it lists your monitors with their names
  and sizes, all ticked, so you choose which ones the Frame shows. It installs `cc-host`, one
  static binary for x86_64 or aarch64, and tells you what still needs you, such as a firewall rule.
  A Steam Deck in desktop mode counts as a computer: SteamOS 3.8 comes with krdp.
- **Pairing** is the last step, and the installer walks you through it. Run `cc-share pair` on
  the computer and a 6-digit key fills its screen. On the Frame, open Workspace, then Machines,
  then Add machine, pick the computer and press Pair. Then type the key, or press Pair by
  looking and look at the screen. If the computer isn't in the list, press Pair by looking anyway:
  the key screen shows the computer's address as tags too, so the Frame reads both. Typing the
  address works as well.
  Esc, a click or a tap closes the key screen, so a Steam Deck with no keyboard can cancel too. **Align** (Machines, Align) then puts each panel on the real
  monitor it shows.

If there's no terminal to answer questions on, pass the answers instead: `| sh -s -- --yes` installs or
updates, `| sh -s -- --yes 0 1` shares monitors 0 and 1, `| sh -s -- --remove --yes` removes
it, and `| sh -s -- --dry-run` only says what it would do. To build a checkout by hand, run
`./install.sh` in it.

Nothing between the Frame and a computer uses SSH. It all goes through pairing, the computer's
agent (TLS on port 3399) and krdp ([docs/ssh-free.md](docs/ssh-free.md)).

### Signed releases

Releases and the Arch packages are signed with Command Center's own key. Its fingerprint is
`79BF A59F 256A 889D 2152  0E84 60EB 1BE5 E677 4107`, and the public key is
[packaging/command-center.asc](packaging/command-center.asc) (also on
[framecc.nomadsgalaxy.com](https://framecc.nomadsgalaxy.com/command-center.asc)). If a fingerprint
anywhere else doesn't match this one, don't trust it.

### What a computer shares and leaves open

- **Announcing** broadcasts the computer's name and its monitors' outputs and sizes to the local
  network (mDNS), so the Frame can list it. There's no login in it and no address beyond what mDNS
  already shows. It's ticked in the installer, so untick it if you'd rather not. `--yes` leaves it
  as it was, which is off on a new install. It needs `avahi-daemon` running, and `cc-share check`
  says when it isn't. A Steam Deck has it off: `sudo systemctl enable --now avahi-daemon` turns it
  on. SteamOS also turns publishing off in `/etc/avahi/avahi-daemon.conf`, so a Deck still won't
  be listed: use Pair by looking, or type its address in Add machine.
- **The firewall rule** lets the private ranges (10/8, 172.16/12, 192.168/16) reach ports 3399-3449.
  That's wider than your subnet, so on a large private network or a VPN more machines can reach the
  door. Only paired Frames get past it. With firewalld, the rule goes into each active zone except
  public, external, dmz, block and drop, since a network in one of those isn't one you trust. If
  your home network is in one of them, the installer opens nothing and says how to move it to the
  home zone (`sudo firewall-cmd --permanent --zone=home --change-interface=<interface>`). With ufw
  there are no zones, so it's the one rule.
- **The shared login (slot 0)** from before pairing stays on (ports 3400+), and anyone with its
  password can connect. `cc-share check` and `cc-share frames` remind you while it's there. Once
  every monitor you use is paired, you can retire it, and the Machines window will offer to.

On the computer, `cc-share check` runs the checklist again (`cc-share install … --dry-run` shows what
an install would change), `cc-share frames` lists paired Frames, `cc-share unpair <frame>` revokes
one, and `cc-share lock` / `unlock` pauses the agent.

**Diagnosing with SSH:** SSH is never used by default. With `CC_SSH=1`, cc-home may use it (align's
old path, `machine probe user@host`), and it says so every time. cc-home's `tests/nossh.rs`
(`cargo test -p cc-home --test nossh`) proves that everything else works with SSH gone.

## Spots (saved screen places)

`cc-home` saves where the screens are in the room (SteamVR's standing space), so they come back to
the same real-world place whichever way you're facing. Screens stay free to move and recentering
still works. Spots only apply when you ask.

```
cc-home save desk          # save every screen where it is now (default spot: home)
cc-home apply desk         # put them back
cc-home list | forget <spot>
cc-home calibrate desk-wide   # "home" for a remote monitor: touch three corners of its picture
```

Calibrating puts a remote monitor's panel exactly on the real monitor, at its real size. When you
touch the corners (`cc-home calibrate`), it reads the controller's tip from the running Desktop
(cc-panels). Spots live in `~/.config/control-center/home.json`. If SteamVR's room origin moves
(say, you redo room setup), save or calibrate again.

**Align** does the same from a look. The monitor shows AprilTags, the headset's view
(`/dev/video99`, the VR mirror with passthrough) reads them while you move your head slowly, and
each panel lands on its real monitor, curve included. Use Machines, Align, or
`cc-home machine align <monitor>` ([docs/apriltag-mapping.md](docs/apriltag-mapping.md)). After an
align, a monitor you've moved by hand gets a button beside its grab bar that puts it back. If
SteamVR relocalizes the room, everything shifts together, so align one monitor and move the rest
with it (`cc-home reanchor <monitor>`).

## Not yet

- **Steam Deck:** its desktop mode is X11 and Game Mode is gamescope, and krdp can't capture
  either. Sunshine + Moonlight would cover it.
- **Gaming-grade latency:** RDP suits desktop work. Moonlight is the upgrade path.

## License

Command Center is under OCL v1.1 + SWAtt v1 (Open Community License v1.1 + Software Attribution
v1, by Prusa Research). Creator: Nomads Galaxy. See [NOTICE.md](NOTICE.md), [LICENSE](LICENSE)
and [LICENSE-SWAtt-v1.md](LICENSE-SWAtt-v1.md). Third-party components keep their own licenses.
