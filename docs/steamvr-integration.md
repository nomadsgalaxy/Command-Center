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

## Moving and resizing windows

Every panel works the way SteamVR's own windows do, with the trigger (never the grip, which the
Frame keeps for switching a controller between gamepad and hands). What changed for you:

- **New windows open 1.4 m in front of you,** level and facing you, and stay where you put them.
  Before, window panels opened at 1.2 m, and a computer's screen with no saved place opened at a
  fixed spot in the room, whichever way you faced. The Machines, Preferences and Workspace windows
  still open close by, at 0.6 m, and a pop-out still opens in front of its monitor.
- **Push and pull speed up.** While you carry a panel, keep the thumbstick (or the wheel) going
  one way and it moves faster the longer you hold it, up to four times as fast after a second
  and a half. Let go of the stick or turn back and it starts slow again. It stops at 0.25 m and 5 m.
- **The bottom-right corner resizes about the centre.** It's the resize handle, as on SteamVR's
  windows, and the panel now grows and shrinks around its middle instead of hanging from its
  top-left corner. A window still takes any shape there, and a computer's screen keeps its
  monitor's shape. The other three corners work as before.
- **Tap the curve button** (beside the bar) to flip a panel between flat and curved round you.
  Holding it and pulling, or the thumbstick while it's held, still bends it as finely as you like.
- **Dock a panel on a hand or your head** (off for now). With `"docking": true` in
  `~/.config/control-center/settings.json`, carry a panel to your other controller and hold it
  there until the controller buzzes (about half a second), then let go: it rides on that
  controller, pulled in to within 60 cm of it. Do the same with your hand at your face and it rides
  on your head, between 40 cm and 2 m away. Passing by without the buzz doesn't dock. Grab it again
  to take it off; let go anywhere else and it stays in the room. If the controller goes off or
  loses tracking for half a second, the panel stays in the room where it was. Docking lasts until
  the Desktop closes, and the panel's saved place stays where it last was in the room, even if you
  bend or resize it while it's docked. If the headset loses tracking, a panel on your head stays
  put on it; a controller that's switched off leaves its panel in the room straight away.

  Docking stays off by default until two things are done. You'll see the dock spots while you
  carry something. And SteamVR will move a docked panel itself, tied to the controller or
  headset (a device-relative transform), instead of Command Center moving it once a frame,
  which can trail a fast hand a little.

Everything else stays: face me, snap back, the curve button's pull, zoom, the other corners,
Right Ctrl and the wheel. The card and its buttons still fade in when a laser comes near and out
after it leaves.

### Docking: checks in the headset

With `"docking": true`:

- [ ] The carrying hand buzzes when a dock arms (about half a second in the spot), and only once.
  If the other hand buzzes, or neither does, the buzz needs its own output action instead.
- [ ] Passing a hand by the other controller without waiting doesn't dock.
- [ ] A panel docked on a hand from across the room comes in to about 60 cm and turns with the
  wrist without swinging wide.
- [ ] A docked panel keeps up with a quick hand move (no visible trailing). If it trails, that's the
  device-relative blocker above.
- [ ] Switching the controller off leaves the panel where it was, at once; covering the
  controller's tracking for a moment doesn't.
- [ ] Taking the headset off and on keeps a head-docked panel on the head.

## The look

Command Center uses SteamVR's own colours: its dark greys (#24272d, #3d4450, #464d58), its light
text (#dfe3e6), its blue (#1a9fff) on the dashboard tab, and its orange-red (#ff4500) for close.
Text is in Noto Sans and typed text in Source Code Pro, both from the system's fonts. Valve's own
font, logos and icons aren't used anywhere: our buttons' glyphs (curve, zoom, face me, snap back,
the keyboard) are our own, drawn flat. The panels keep their own accents, cyan for a computer's
screens and violet for the Frame's windows, so you can still tell them apart.

To have it follow your Plasma session's colour scheme and font instead, as it used to, put this
in `~/.config/control-center/settings.json` and restart the Desktop:

```json
{ "look": "plasma" }
```

## Theater mode and new windows' size

A window in theater mode opens 1.8 m ahead of you, 2 m wide and curved. SteamVR's own theater
screen is flat and 3 m away, at its default scale of 2.8. SteamVR's scale has no fixed metres
(it multiplies a page's own size), so the two can only be compared by how wide they look: if
SteamVR's page is 1.2 m wide at scale 1, which nobody has measured yet, both fill about 58° of your
view. So nothing changed here. New windows open 1.4 m ahead (see above), a computer's screen with
no saved place 1.2 m wide, and a window panel as wide as its window's pixels make it.

## Where things come back after a restart

A panel comes back to the last place you put it in the room you're in now. If you've never put
it anywhere in this room (a new room, or travelling), it opens in front of you instead, to place
yourself: another room's places are never carried over, since there they'd be somewhere else
entirely. A room with its own workspace keeps its places; Temporary, the travel workspace, keeps
each room's own places separately (for the last 8 rooms it's been used in), so going from one
room to another and back brings each room's layout back, and a room it hasn't seen starts empty. A window
panel's own last place (kept while its window stays open, and the one an app reopened after the
Desktop was closed comes back to) follows the same rule. The Machines, Preferences and Workspace
windows too: with no place for them in this room, they open 1.4 m in front of you like
everything else.

Docking doesn't change any of this: a docked panel's place in the room stays where you last put
it down, so after a restart it's there (in the same room) or in front of you (somewhere new).

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
- On a panel that takes characters, every character goes as a character, plain letters and
  space too. That's right for typing text, but a program that reads keys rather than text (a
  game's space to pause, WASD to move) won't see them from SteamVR's keyboard. Use a real keyboard
  for those.
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
- [ ] krdp really types `é€😀` from Unicode events, not only says it takes them (its input
      capability) and then drops them. Until this passes, krdp isn't supported for typing as
      characters.
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
