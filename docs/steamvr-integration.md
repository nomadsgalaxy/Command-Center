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
the places from the room you last laid it out in, and starts empty in a different one. A window
panel's own last place (kept while its window stays open) follows the same rule.

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
- It types as a US keyboard does. A character that isn't on one (an accented letter, the euro
  sign, an emoji) isn't sent, and the strip says "Couldn't type 1 character" for a few seconds.
  It never says which character, and neither does the log, since you might be typing a password.
- The remote computer reads the keys with its own layout, the same as a real keyboard's keys. If
  it isn't set to US, some punctuation comes out as what's on that key in its layout.
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
