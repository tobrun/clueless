#!/usr/bin/env bash
# Create the self-signed codesigning identity "clueless-dev" that
# `cargo xtask run` signs the dev bundle with, so macOS keeps the Microphone,
# Screen Recording and Local Network grants across rebuilds: TCC pins grants
# to the designated requirement derived from the signing identity, so a stable
# identity keeps them attached (see docs/decisions.md).
#
# Layout (learned the hard way on macOS 15 + OpenSSL 3):
#   * A build keychain (~/Library/Keychains/clueless-build.keychain-db) holds
#     the identity instead of the login keychain. Its passphrase lives in a
#     0600 file next to the cert, which lets `security set-key-partition-list`
#     run unattended - without it codesign gets a GUI "app wants access to
#     key" prompt (or errSecInternalComponent under -) on every build.
#   * A root CA signs the leaf because `security import` refuses to set key
#     partition-list entries for self-signed certificates, and codesign
#     requires a chain to a trusted root.
#   * The leaf lives 825 days: Gatekeeper rejects signing certificates with
#     longer validity, and a re-issued cert keeps the same CN so TCC treats it
#     as the same identity.
#
# You will be asked for your login/admin password once, for trusting the root
# CA. Everything else runs unattended afterwards.
#
#   scripts/make-dev-cert.sh            create the identity (idempotent)
#   scripts/make-dev-cert.sh --dry-run  print every command, touch nothing
set -euo pipefail

IDENTITY="clueless-dev"
CA_NAME="Clueless Dev Root CA"
LEAF_DAYS=825
CERT_DIR="$HOME/Library/Application Support/clueless-dev-cert"
BUILD_KC="$HOME/Library/Keychains/clueless-build.keychain-db"

dry_run=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) dry_run=1 ;;
    *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
  esac
done

run() {
  if [ "$dry_run" = 1 ]; then
    printf 'would run:'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

# Redirect a command's stdin from /dev/tty so GUI authorisation prompts still
# appear; `security` otherwise sometimes bails with a CSSM error on pipes.
tty_run() {
  if [ "$dry_run" = 1 ]; then
    printf 'would run:'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@" < /dev/tty
  fi
}

if [ "$dry_run" = 0 ] && \
   codesign --force --sign "$IDENTITY" /tmp/.clueless-sigtest.$$ >/dev/null 2>&1; then
  rm -f /tmp/.clueless-sigtest.$$
  echo "identity $IDENTITY already signs, nothing to do"
  exit 0
fi
rm -f /tmp/.clueless-sigtest.$$

if [ "$dry_run" = 0 ]; then
  mkdir -p "$CERT_DIR"
  chmod 700 "$CERT_DIR"
fi

# 1. Root CA + code-signing leaf, issued by the CA.
run openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout "$CERT_DIR/ca.key" -out "$CERT_DIR/ca.crt" \
  -subj "/O=Clueless Dev/CN=$CA_NAME" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign"

run openssl req -newkey rsa:2048 -nodes \
  -keyout "$CERT_DIR/leaf.key" -out "$CERT_DIR/leaf.csr" \
  -subj "/O=Clueless Dev/CN=$IDENTITY"

if [ "$dry_run" = 0 ]; then
  printf 'basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\nsubjectAltName=email:dev@clueless.local\n' > "$CERT_DIR/leaf.ext"
fi

run openssl x509 -req -in "$CERT_DIR/leaf.csr" \
  -CA "$CERT_DIR/ca.crt" -CAkey "$CERT_DIR/ca.key" -CAcreateserial \
  -days "$LEAF_DAYS" -sha256 -extfile "$CERT_DIR/leaf.ext" \
  -out "$CERT_DIR/leaf.crt"

# Legacy PKCS#12 ciphers: `security import` rejects OpenSSL 3's AES/SHA-256
# defaults ("MAC verification failed ... PKCS12_verify_mac").
kc_pass=""
if [ "$dry_run" = 0 ]; then
  if [ -f "$CERT_DIR/keychain.pass" ]; then
    kc_pass=$(cat "$CERT_DIR/keychain.pass")
  else
    kc_pass=$(openssl rand -hex 16)
    printf '%s' "$kc_pass" > "$CERT_DIR/keychain.pass"
    chmod 600 "$CERT_DIR/keychain.pass"
  fi
fi
run openssl pkcs12 -export -name "$IDENTITY" \
  -inkey "$CERT_DIR/leaf.key" -in "$CERT_DIR/leaf.crt" \
  -certfile "$CERT_DIR/ca.crt" -out "$CERT_DIR/leaf.p12" \
  -passout "pass:$kc_pass" \
  -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1

# 2. The build keychain: created with its own passphrase so the partition
#    list can be set unattended (step 3).
run security create-keychain -p "$kc_pass" "$BUILD_KC"
run security set-keychain-settings -u "$BUILD_KC"
run security import "$CERT_DIR/ca.crt" -k "$BUILD_KC" -A
run security import "$CERT_DIR/leaf.p12" -k "$BUILD_KC" -P "$kc_pass" \
  -f pkcs12 -T /usr/bin/codesign

# 3. Grant codesign (and the usual tools) access to the private key without
#    a per-build unlock prompt. This is what fails on the login keychain,
#    where the passphrase is not available to scripts.
run security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -k "$kc_pass" "$BUILD_KC"

# 4. Make codesign see the identity: the build keychain joins the user
#    search list (after the login keychain, which stays first).
if [ "$dry_run" = 0 ]; then
  if ! security list-keychains -d user | grep -qF "$BUILD_KC"; then
    old=$(security list-keychains -d user | tr -d '"')
    # shellcheck disable=SC2086
    run security list-keychains -d user -s $BUILD_KC $old
  else
    echo "build keychain already on the search list"
  fi
else
  run security list-keychains -d user -s '"$BUILD_KC" <existing...>'
fi

# 5. Trust the root CA. This is the one step that asks for the login
#    password; the trust settings live in the keychain domain.
tty_run security add-trusted-cert -r trustRoot -p basic -p codeSign \
  -k "$BUILD_KC" "$CERT_DIR/ca.crt"

if [ "$dry_run" = 1 ]; then
  echo "dry run: nothing was created and the keychain was not touched"
  exit 0
fi

rm -f "$CERT_DIR/leaf.p12" "$CERT_DIR/leaf.key" "$CERT_DIR/ca.key" \
  "$CERT_DIR/leaf.csr" "$CERT_DIR/leaf.ext"

echo
echo "verifying a real signature:"
echo "test" > /tmp/clueless-sigtest
codesign --force --sign "$IDENTITY" /tmp/clueless-sigtest
codesign -dvv /tmp/clueless-sigtest 2>&1 | grep -E '^Authority' || true
rm -f /tmp/clueless-sigtest
echo
echo "done. 'cargo xtask run' now signs the dev bundle as $IDENTITY."
