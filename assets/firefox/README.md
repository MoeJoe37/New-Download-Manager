# Signed Firefox add-on

`ndm.xpi` is the unchanged Mozilla-signed, unlisted release. Its version, SHA-256,
source revision and original Mozilla download URL are recorded in `signed.json`.
NDM embeds these bytes and exports `NDM-Firefox.xpi` for permanent installation.

Never unzip, rewrite, or repackage this file: doing so invalidates the signature.
When extension code or permissions change, submit a new Firefox version to
Mozilla, replace the approved XPI and update its metadata. App-only version
bumps do not require signing unchanged extension code again.

`python3 tools/test-signed-extension.py` checks the archive against the source.
CI also installs the exported XPI in release Firefox with signature enforcement
and verifies that it is still active after restarting Firefox.
