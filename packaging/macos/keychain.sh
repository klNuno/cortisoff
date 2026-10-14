#!/bin/sh
# The release signing identity, in a keychain made for one CI job.
#
#   packaging/macos/keychain.sh import   needs MACOS_SIGNING_P12 (base64 of
#                                        the .p12) and MACOS_SIGNING_PASSWORD
#   packaging/macos/keychain.sh remove   run it in an always() step
#
# import creates and unlocks a keychain under $RUNNER_TEMP, imports the
# identity for codesign alone, and writes CORTISOFF_SIGN_IDENTITY (the
# certificate's SHA-1, which is how codesign is told to use a self-signed
# identity it does not trust) and CORTISOFF_KEYCHAIN to $GITHUB_ENV, for
# bundle.sh. Without the secrets (a fork, a pull request from outside) it
# writes nothing and bundle.sh signs ad hoc. Nothing secret is printed.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
keychain="${RUNNER_TEMP:-/tmp}/cortisoff-signing.keychain-db"

case "${1:-}" in
import)
  if [ -z "${MACOS_SIGNING_P12:-}" ] || [ -z "${MACOS_SIGNING_PASSWORD:-}" ]; then
    echo "No signing identity in this run: the bundle is signed ad hoc."
    exit 0
  fi
  # The identity the committed certificate names. A secret holding any
  # other identity fails here rather than shipping a new requirement.
  identity=$(openssl x509 -in "$here/signing.pem" -noout -fingerprint -sha1 |
    sed 's/^.*=//; s/://g' | tr 'a-f' 'A-F')
  p12="$(dirname "$keychain")/cortisoff-signing.p12"
  # Only this job ever unlocks it.
  unlock=$(openssl rand -hex 24)
  trap 'rm -f "$p12"' EXIT
  umask 077
  printf '%s' "$MACOS_SIGNING_P12" | base64 --decode > "$p12"
  security create-keychain -p "$unlock" "$keychain"
  security set-keychain-settings -lut 3600 "$keychain"
  security unlock-keychain -p "$unlock" "$keychain"
  security import "$p12" -k "$keychain" -f pkcs12 -P "$MACOS_SIGNING_PASSWORD" -T /usr/bin/codesign > /dev/null
  security set-key-partition-list -S apple-tool:,apple: -s -k "$unlock" "$keychain" > /dev/null
  # shellcheck disable=SC2046 # one keychain path per word, as security prints them
  security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
  # Without -v: a self-signed certificate is not trusted, and -v hides
  # every identity that is not.
  if ! security find-identity -p codesigning "$keychain" | grep -q "$identity"; then
    echo "::error::The signing secret does not hold the identity of packaging/macos/signing.pem ($identity)."
    exit 1
  fi
  echo "Signing identity $identity imported."
  {
    echo "CORTISOFF_SIGN_IDENTITY=$identity"
    echo "CORTISOFF_KEYCHAIN=$keychain"
  } >> "$GITHUB_ENV"
  ;;
remove)
  if [ -f "$keychain" ]; then
    security delete-keychain "$keychain"
    echo "Signing keychain removed."
  fi
  ;;
*)
  echo "usage: keychain.sh import|remove" >&2
  exit 2
  ;;
esac
