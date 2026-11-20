# Security

Eidolon seals a folder into an encrypted bundle, moves it between the machines on a holder list with one holder at a time, and opens it without letting the bundle write outside its folder. If you find a way to break one of those guarantees, for example a bundle that writes outside its folder, a seat accepted from a machine that did not hold the one before, or a bundle opened by a key it was not encrypted to, report it privately through this repository's security advisories (the **Report a vulnerability** button under **Security**), not in a public issue or pull request.

Say what you found, how to reproduce it, which version you ran (`eidolon --version`), and on which system. A proof of concept helps, as text or a short script. You will get an answer within seven days. Eidolon is at 0.x, so fixes go into the latest release only.

The scope is this repository: the `eidolon` command and the crates under `crates/`. age, zstd, tar, OpenSSH and the key stores are their projects' own.
