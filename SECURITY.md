# Security

Eidolon keeps everything that makes one entity present (a folder, a browser profile, keys) as one sealed thing that opens in an Adytum cell and moves between machines, sending only what changed. If you find a way to make Eidolon cross a boundary it should hold, tell Will before telling anyone else.

## Reporting a problem

Email will@willctl.com. Don't open a public issue or pull request for a security problem, so the fix can be out before the problem is.

Say what you found, how to reproduce it, which version you ran (`eidolon --version`) and which operating system. Say what had to be running already, under which account, and what it could read or change. A proof of concept helps, as text or a short script.

You'll get an answer within seven days. Eidolon is at 0.x: fixes go into the latest release only.

## What's in scope

Everything in this repository: the `eidolon` command and the crates under `crates/`.
