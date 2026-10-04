<img src="packaging/command-center.png" alt="" width="96">

# Command Center

Command Center is a VR desktop for the Steam Frame. Your windows, and the monitors of your other
computers, hang in the room around you as panels you can move, resize and curve. One mouse and
keyboard drive whichever panel you point at. It's all Rust. More at
[framecc.nomadsgalaxy.com](https://framecc.nomadsgalaxy.com).

## What you need

- A Steam Frame with SteamOS and SteamVR.
- A Bluetooth mouse and keyboard paired with the Frame. For now they're required.
- Computers running Linux with KDE Plasma 6 on Wayland and krdp, on the same local network.
  A Steam Deck in desktop mode counts.

## Install

Run this in a terminal on the Frame, then on each computer you want to see from it:

```
curl -fsSL https://framecc.nomadsgalaxy.com/install | sh
```

The installer works out which machine it's on. Run it again later to update or remove Command
Center.

- **On the Frame,** it builds Command Center. The first run takes a while. When it's done, open
  **Desktop** from the SteamVR launcher.
- **On a computer,** you pick which monitors the Frame can show. If something would stop the
  Frame from finding or reaching the computer, like the firewall, the installer shows the exact
  `sudo` commands to fix it and asks first. The details are in
  [docs/packaging.md](docs/packaging.md#host-fixes-what-the-installer-finds-and-fixes).

With no terminal to answer questions on, pass the answers: `| sh -s -- --yes` installs or
updates, `--remove --yes` removes, and `--dry-run` only says what it would do.

## Pair and align

1. On the computer, run `cc-share pair`. A 6-digit key fills its screen.
2. On the Frame, open **Workspace → Machines → Add machine**. Pick the computer, or just press
   **Pair by looking** and look at its screen: the key screen carries the computer's address too,
   so it doesn't need to be in the list. You can also type the address and key.
3. Press **Align**. The monitors show AprilTags, the headset's camera finds them, and each panel
   lands on its real monitor.

## Everyday use

- **Spots** save where your panels are in the room. `cc-home save desk` saves them, and
  `cc-home apply desk` puts them back.
- **Workspaces** keep a layout per room. Back in a room, **Enter workspace** uses one of its
  monitors to put everything where it belongs.
- **Closing the Desktop** saves which apps were open and frees the memory for a game. Opening
  it again brings them back.

On a computer, `cc-share check` says if anything's wrong and how to fix it, and `cc-share frames`
lists the Frames paired with it.

## Security

- The Frame and a computer only talk after pairing, through the computer's agent (TLS on port
  3399) and RDP (ports 3400 and up).
- The installer opens those ports for private networks only, and never in an untrusted firewall
  zone.
- Releases are signed. The key's fingerprint is
  `79BF A59F 256A 889D 2152  0E84 60EB 1BE5 E677 4107`, and the public key is
  [packaging/command-center.asc](packaging/command-center.asc). If a fingerprint anywhere else
  doesn't match, don't trust it.

## Not yet

- **Controlling a Steam Deck out of the box:** SteamOS's own krdp shows the screen but drops
  mouse and keyboard input. `krdp/deck/build.sh` builds a fixed one
  ([docs/packaging.md](docs/packaging.md#steamos-hosts)); the installer doesn't do it for you yet.
- **Gaming-grade latency:** RDP suits desktop work, not fast games.

## License

OCL v1.1 + SWAtt v1 (Open Community License v1.1 + Software Attribution v1, by Prusa Research).
Creator: Nomads Galaxy. See [NOTICE.md](NOTICE.md), [LICENSE](LICENSE) and
[LICENSE-SWAtt-v1.md](LICENSE-SWAtt-v1.md). Third-party components keep their own licenses.
