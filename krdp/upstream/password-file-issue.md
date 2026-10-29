# krdp: let krdpserver read the password from a file or stdin, not only `-p`

**Component:** krdp (krdpserver), seen on 6.7.5 and 6.8.0

## What happens

The only way to give `krdpserver` a username and password without the KCM and the keyring is
`-u <user> -p <password>`. That puts the password in the process's argv, and on Linux
`/proc/<pid>/cmdline` is readable by every local user unless `/proc` is mounted with `hidepid`.
So anyone logged in to the machine can read it with `ps -ef` or `cat /proc/<pid>/cmdline`, the
whole time the server runs.

This matters when krdpserver is started by a script or a systemd user unit, which is the usual way
to run it headless or to run several of them (one per monitor, say). The KCM path isn't always
there: there's no keyring unlocked in a session started from a unit, and we want one password per
server.

## What we'd like

Either of these, next to `-p` (which keeps working):

- `--password-file <path>`: read the password from the first line of the file, without its trailing
  newline. It requires `--username`, like `-p` does, and fails with a clear message naming the path
  if the file can't be read. Pointing it at `/dev/fd/N` also lets a parent hand the password over an
  inherited pipe, with no file on disk.
- Or `--password-stdin`: read it from the first line of stdin.

With `--password-file`, a systemd unit can also use `LoadCredential=` and
`--password-file ${CREDENTIALS_DIRECTORY}/password`.

## What we use now

We carry a small patch for `server/main.cpp` that adds `--password-file` as above (about 15 lines:
the option, a `QFile` read of the first line, and passing that to `KRdp::User::password`). We'd be
glad to send it as a merge request if the option sounds right to you.
