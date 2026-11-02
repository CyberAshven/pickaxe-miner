#!/usr/bin/env bash
# #### PR #42: HiveOS sources this with the flight sheet in scope. The fields
# are kept for h-run.sh, NUL-separated and owner-only: a node URL in Pool URL
# may carry its RPC login. Extra config becomes the miner's extra flags.
# shellcheck disable=SC2154 # CUSTOM_* come from HiveOS and h-manifest.conf.
umask 077
printf '%s\0' --pool "$CUSTOM_URL" --user "$CUSTOM_TEMPLATE" --password "$CUSTOM_PASS" \
  > "$CUSTOM_CONFIG_FILENAME"
printf '%s' "$CUSTOM_USER_CONFIG" | tr '\n' ' ' > "$CUSTOM_CONFIG_FILENAME.extra"
