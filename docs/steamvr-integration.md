# Working like SteamOS

Command Center tries to behave like the rest of SteamVR on the Frame: the same bar, keyboard,
notifications and window handling you get from Valve's own windows, wherever SteamVR gives an
app a way to use them. This page says what we use, where, and how to turn it off.

## SteamVR's control bar

SteamVR draws its own bar, with a close button, under Command Center's tab in the dashboard.
Close quits Command Center, the same as quitting any other overlay app from the dashboard.

The floating windows (remote screens, window panels, pop-outs, and the Machines, Preferences and
Workspace windows) keep our own bar, buttons and card: move, resize, curve, dock, face me and the
rest. That's because SteamVR only draws its bar for what its dashboard hosts. Valve's header says
the bar is "drawn underneath of it in the dashboard", and SteamVR's dashboard code only reads the
close and keyboard flags from its own frames. A window we place in the room isn't one of those.

To turn the bar off, put this in `~/.config/control-center/settings.json` and restart the Desktop:

```json
{ "steamvr_bar": false }
```

## The dashboard tab, always there

Command Center's tab sits in SteamVR's dashboard next to Steam's whether the Desktop is open or
not. With the Desktop closed it's a small process SteamVR starts with itself, and the tab says
"The Desktop is closed. Show opens it." Pressing **Show** opens the Desktop the same way the
launcher's tile does, and once it's up the Desktop's own tab takes over in the same spot.

It gets set up the first time you open the Desktop (or by `cc-home install` while SteamVR runs).
If you'd rather SteamVR didn't start it, turn it off in SteamVR's settings, under its startup
apps, and it stays off. `cc-home install remove` takes it out.

What it costs: one OpenVR connection each time SteamVR starts, and the memory of one idle process.
It has no action manifest and takes no controller input except the laser on its own page.

## SteamVR's keyboard

Every live panel (a computer's screen, a window, a pop-out) has a keyboard button on its bar,
right of the curve button and, on a window, the zoom button. Press it and SteamVR's keyboard
opens for that panel. Each key goes to the computer or window the moment you press it, the same
way a real keyboard's keys go, and Backspace and Enter work too. Press the button on another panel
to type there instead.

SteamVR's keyboard has no Esc, Tab, arrow keys, Ctrl, Alt or Super, so while it's open a key
strip of ours sits under the panel with those. Ctrl, Alt and Super are sticky: press one (it
lights up) and it goes with your next key, from either keyboard, or your next click on that
panel, so Ctrl and then C on SteamVR's keyboard is Ctrl+C. Anything Command Center holds down
for you is let go when the key or click ends, when the keyboard closes, when you type somewhere
else, and when the panel's connection drops.

What it can't do:

- Function keys. Use a real keyboard for those.
- What you type goes as the characters themselves where the panel can take that: a VNC screen
  always, and an RDP screen when its server says it takes Unicode input. Then the computer's own
  keyboard layout doesn't matter, and accented letters, the euro sign and emoji all work. Window
  panels, and RDP servers that don't take Unicode, get a US keyboard's keys instead, and the
  keyboard's hint and the strip say "as a US keyboard" before you type. On those, a character a US
  keyboard doesn't have isn't sent, and the strip says "Couldn't type 1 character" for a few
  seconds. It never says which character, and neither does the log, since you might be typing a
  password. A character our side couldn't send (the connection dropped, or the RDP server
  refused it) counts the same way, and it isn't sent again some other way.
- That promise is only Command Center's side. Once a character reaches the computer, its own
  remote desktop server types it: krdp and KRFB on Wayland go through the desktop portal, and
  KRFB on X11 looks the character up on the host's keyboard layout (XTest). Any of them can drop
  a character the host's layout has no key for, and we can't see that from here.
- Backspace, Enter and Tab, the strip's keys, and anything you type with a sticky Ctrl, Alt or
  Super are keys, not characters, because a shortcut means a key. They go by the US layout's key
  positions, the same as a real keyboard's keys do.
- Press a key on a real keyboard while it's open and it closes, since you've gone back to the
  real one. Pressing the keyboard button always opens it, even right after you typed on a real
  keyboard.

The text fields in the Machines and Workspace windows open SteamVR's keyboard when you click
them. If you've pressed a key on a real keyboard in the last minute, they don't, and the real
keyboard types into the field instead.

## Notifications

Command Center tells you in the headset when:

- a pairing worked, or didn't, and why;
- something in the Machines window failed (adding, removing or renaming a machine);
- a computer stopped sharing a screen (it gave up restarting it), keeps restarting it, or is back;
- a different computer answers at a paired machine's address, so it needs pairing again.

They're ordinary desktop notifications, sent to Steam's own notification service on the Frame
(org.freedesktop.Notifications on your user bus), the same way other apps on the device notify
you. If that service doesn't answer, the note shows as a SteamVR toast instead.

They don't pile up. One thing, like a screen's health, says something at most every 30 seconds,
and if it changes again meanwhile you get only the latest. No more than four go out in a minute.

To turn them off, add this to `~/.config/control-center/settings.json` (it applies straight away):

```json
{ "notifications": false }
```

Whether they show in the headset hasn't been checked on every SteamOS version yet. To try it,
open the Desktop and run `cc-home notify-test` for a notification, or `cc-home notify-test toast`
for SteamVR's toast on its own.

### Typing: checks before calling it supported

Typing characters as themselves (RDP Unicode input, VNC keysyms) is built and unit-tested, but it
hasn't been tried against real hosts yet, so it isn't supported until this list passes on the
desktop. For each of krdp and a VNC server, and for each host layout (US and German):

- [ ] `zyYZ` comes out as typed (a German layout swaps Z and Y as keys).
- [ ] Punctuation: `@"#$%^&*()[]{};:'<>?/\|`~`.
- [ ] `€£éß` and an emoji (😀) come out as typed, or on US-keys panels the strip counts them as
      not typed and nothing else appears.
- [ ] Backspace deletes one character, Enter makes one new line (also for a \r\n), Tab moves on.
- [ ] Sticky Ctrl then `c`, `v`, `a` on SteamVR's keyboard copy, paste and select all. Sticky Alt
      then Tab switches windows. Sticky Super opens the launcher.
- [ ] A sticky Ctrl held through a click on the panel makes a Ctrl-click, and nothing stays
      pressed after it (type a letter: it's lowercase and plain).
- [ ] Focus loss: laser off the panel mid-click, then type: no modifier stuck.
- [ ] Reconnect: drop the network for a few seconds while the keyboard is open, then type: it
      types, nothing stuck, and an RDP server that doesn't take Unicode still says "as a US
      keyboard".
- [ ] A window panel says "as a US keyboard" and types ASCII.
- [ ] On a US host: `€` and `ł` come out, or are counted as not typed. Nothing else appears.
- [ ] An emoji (a surrogate pair over RDP) comes out as one character.
- [ ] On a German host: `ß ä ö ü é`, and a dead-key character (`^` or `´` followed by a letter),
      come out as typed.
- [ ] Sticky Ctrl plus a non-US character counts as not typed, and Ctrl isn't stuck afterwards
      (the next letter is plain).
- [ ] Paste 500 characters through SteamVR's keyboard: all arrive, in order, none dropped.
- [ ] With Caps Lock on at the host, typed characters come out as typed (not swapped case) on
      Unicode panels.
- [ ] After a reconnect, an RDP panel's "as a US keyboard" note (or its absence) is the same as
      before (`rdp::unicode()` read again from the new connection).
