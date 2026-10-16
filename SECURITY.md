# Security

simple-sync is **early-stage software and has not been independently audited.** Use it for collaborators you trust and documents that are not highly sensitive. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the security model and its known limits.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Use GitHub's **Report a vulnerability** button (Security tab of this repository) to send a private report. Include what you found, how to reproduce it, and the version. Reports are read as time allows; there is no bounty and no guaranteed response time.

## What to expect from the design (summary)

- Links (`ssync://`) are bearer secrets: anyone holding one can read the document, and an Edit link can also change it.
- Traffic is end-to-end encrypted and edits are signed, but all editors share one signing key and there is no forward secrecy.
- While sharing, the app opens a UDP port and contacts public relay/discovery servers; your IP address is visible to peers and to those servers.
