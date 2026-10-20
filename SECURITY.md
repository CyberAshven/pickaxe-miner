# Security Policy

## Reporting a vulnerability

Please report suspected security vulnerabilities **privately** to **BioTechWarden@protonmail.com**. Do not post exploitation details in public issues or pull requests before coordinating with the maintainer.

Useful reports include the affected version or commit, relevant operating system or GPU backend, threat scenario, attacker-controlled input, expected and observed behavior, and a minimal reproduction or test fixture when available.

Please do not send wallet recovery phrases, private keys, credentials, or funds. Avoid destructive mainnet transactions and testing against infrastructure you do not control.

## Security-sensitive areas

Reports are particularly useful for payout-address integrity; CashToken covenant, baton, and difficulty validation; PHOTON claim-transaction construction; mainnet/Chipnet isolation; handling untrusted Electrum/Fulcrum and node RPC data; and native GPU, driver, FFI, and dependency boundaries.

## Versions and response

The latest published release and current development branches are the relevant codebases for triage. Security fixes may be delivered through a future patch release or coordinated remediation, as appropriate. There is no guaranteed response or disclosure timeline; the maintainer will assess reproducible reports as capacity permits.
