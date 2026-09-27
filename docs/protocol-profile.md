# Contract profile

`protocol/photon.json` is the build-time source for the category ID, redeem bytecode and base mining transaction. The build derives the P2SH32 locking bytecode and Electrum script hash and rejects inconsistent identities. Existing `reference/` files preserve the original deployment.

To adopt an independently reviewed deployment, replace those three fields together, rebuild the miner, and validate the complete path: live baton discovery, GPU search, winner reconstruction, node acceptance, settlement and journal recovery. Use an authorized test deployment first. Static fixtures and compilation alone do not establish compatibility; production promotion requires end-to-end evidence.

The current GPU transaction layout supports a 259-byte redeem script and a 615-byte base mining transaction (age encoding adds up to three bytes). Other lengths are rejected; changed spending rules or transaction layouts require corresponding implementation and end-to-end validation. Updating the profile is quick once compatible deployment details are available, but validation time cannot be guaranteed.

Resolve any pending submission with its matching miner before switching profiles. Journals are bound to the category and script hash, and a mismatched journal is preserved and rejected. Older journals remain bound to the original deployment. Keep the previous binary and journal until any uncertain broadcast outcome has been resolved.
