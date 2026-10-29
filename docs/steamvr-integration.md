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
