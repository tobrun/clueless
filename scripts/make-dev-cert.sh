#!/usr/bin/env bash
# Create the self-signed codesigning certificate "clueless-dev" that
# `cargo xtask bundle` signs the dev app with, so macOS keeps the Microphone,
# Screen Recording and Local Network grants across rebuilds (a stable
# signature keeps the grants attached to the app; see D-dev-signing).
#
# Run this once and enter the login keychain password when asked.
#
#   scripts/make-dev-cert.sh            create the certificate
#   scripts/make-dev-cert.sh --dry-run  print every command, touch nothing
set -euo pipefail

IDENTITY="clueless-dev"
DAYS=3650
CERT_DIR="$HOME/Library/Application Support/clueless-dev-cert"
PEM="$CERT_DIR/clueless-dev.pem"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

dry_run=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) dry_run=1 ;;
    *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
  esac
done

# Run the command, or only print it in dry-run mode.
run() {
  if [ "$dry_run" = 1 ]; then
    printf 'would run:'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

if [ "$dry_run" = 0 ]; then
  if security find-identity -v -p codesigning | grep -qF "\"$IDENTITY\""; then
    echo "identity $IDENTITY already exists in the login keychain, nothing to do"
    exit 0
  fi
  mkdir -p "$CERT_DIR"
  chmod 700 "$CERT_DIR"
fi

# 1. A self-signed certificate with the code-signing extended key usage.
run openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "$CERT_DIR/clueless-dev-key.pem" -out "$PEM" -days "$DAYS" \
  -subj "/CN=$IDENTITY" \
  -addext "extendedKeyUsage=critical,codeSigning"

# 2. Bundle key and certificate into a p12 (ephemeral passphrase, the private
#    key then lives only inside the keychain).
pass=$(openssl rand -hex 16)
run openssl pkcs12 -export -name "$IDENTITY" \
  -inkey "$CERT_DIR/clueless-dev-key.pem" -in "$PEM" \
  -out "$CERT_DIR/clueless-dev.p12" -passout "pass:$pass"

# 3. Import into the login keychain and allow codesign to use it, so signing
#    does not prompt for keychain access on every build.
run security import "$CERT_DIR/clueless-dev.p12" \
  -k "$KEYCHAIN" -P "$pass" -T /usr/bin/codesign

if [ "$dry_run" = 1 ]; then
  echo "dry run: nothing was created and the keychain was not touched"
else
  rm -f "$CERT_DIR/clueless-dev.p12" "$CERT_DIR/clueless-dev-key.pem"
  echo
  echo "identity created:"
  security find-identity -v -p codesigning | grep -F "\"$IDENTITY\"" || {
    echo "warning: $IDENTITY does not show up as a valid codesigning identity yet" >&2
  }
fi

echo
echo "If codesign later rejects the identity as untrusted, run this one"
echo "command once and enter your login password:"
echo
echo "  security add-trusted-cert -r trustRoot -k \"$KEYCHAIN\" \"$PEM\""
