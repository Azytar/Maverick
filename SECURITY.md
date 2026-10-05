# Security Policy

## Reporting a vulnerability

Report it privately, through GitHub's private vulnerability reporting: open the
repository's **Security** tab and use **Report a vulnerability**. This opens a
private advisory that only the maintainers can read. It is not an issue and it
is not visible to other users.

If **Report a vulnerability** is not offered, private reporting has not been
enabled for this repository. In that case, do not open a public issue for an
unpatched vulnerability.

Please give the maintainers a reasonable opportunity to release a fix before
disclosing publicly. That request is about sequencing, not secrecy in the
repository: everything except the vulnerability itself is public.

## What counts as a vulnerability

Maverick is a per-user program that manages an X11 display, and it enforces a
boundary between one local user's session and another's. A report is worth
making when something crosses that boundary, or weakens it:

- Another local user reaching a session's control socket, identity record or log.
- Bypassing the peer-credential check on the control socket.
- An X display cookie being exposed — in a log, an error message, a JSON document
  or any other output.
- A session's X server reachable by another user, or listening where it should
  not be.
- The display claim files under `/tmp` being used to make Maverick act on a path
  it did not create, including through a symlink or a file planted by another
  user.
- A window or client gaining an unintended privilege: managing another user's
  window, spoofing `_NET_WM_PID`, or influencing the claim files above.
- Anything that lets input or a command reach a session that does not belong to
  the sender.

Defects that are not security issues: a window manager that crashes, a layout
that looks wrong, a configuration key that is not honoured, missing documentation,
or a build failure.

## What a useful report contains

- Which Maverick version and commit: `maverick --version`, plus
  `git rev-parse --short HEAD` for a build from source.
- Which local user and display, and whether the display was started by `startx`,
  a display manager, or `maverickctl session`.
- The steps to reproduce, as commands where possible.
- What you observed, and what you expected instead.
- The relevant log. [docs/sessions.md](docs/sessions.md) documents the session
  logs; `maverickctl logs <session>` reads a session's own log.
- Whether the issue needs two different local accounts to observe. If so, say
  what each account attempted.

## What happens after a report

The report is read and assessed, and the reporter is told what was concluded. If
it is accepted, a fix is prepared and a decision is made about release timing,
including whether credit is given. There is no fixed schedule, and no response
time is promised.

Maverick is in preview: the README describes it as not declared production-ready.
Nothing in this policy is a support commitment or a promise of a fix.