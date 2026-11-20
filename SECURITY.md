# Security

Eidolon keeps everything that makes one entity present (a folder, a browser profile, keys) as one sealed thing that opens in an Adytum cell and moves between machines, sending only what changed. If you find a way to make Eidolon cross a boundary it should hold, report it privately through this repository's security advisories (the **Report a vulnerability** button under **Security**), not in a public issue or pull request.

Say what you found, how to reproduce it, which version you ran (`eidolon --version`), and on which system. Say what had to be running already, under which account, and what it could then read or change. A proof of concept helps, as text or a short script. You will get an answer within seven days. Eidolon is at 0.x, so fixes go into the latest release only.

The scope is this repository: the `eidolon` command and the crates under `crates/`.
